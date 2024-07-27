#![no_std]
#![no_main]

use defmt::{info, warn, trace, debug};

use embassy_executor::Spawner;
use embassy_futures::join::{join3, join4};
// use embassy_stm32::exti::ExtiInput;
// use embassy_stm32::gpio::{Input, Level, Output, OutputOpenDrain, Pull, Speed};
// use embassy_stm32::time::Hertz;
// use embassy_stm32::{interrupt, Config, bind_interrupts, peripherals};
use embassy_time::{Duration, Timer};
use embassy_usb::control::OutResponse;
use embassy_usb::Builder;
// use embassy_stm32::usb_otg;
use {defmt_rtt as _};//, panic_probe as _};

use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::Channel;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    loop { }
}

mod xinput;
use crate::xinput::{
    ReportId, RequestHandler, XinputControlReport, XinputReaderWriter, XinputState,
};

use core::convert::Infallible;
use core::panic::PanicInfo;
use embassy_rp::bind_interrupts;
use embassy_rp::clocks::{ClockConfig, UsbClkConfig, UsbClkSrc};
use embassy_rp::config::Config;
use embassy_rp::gpio::{Input, Pull};
use embassy_rp::peripherals::USB;
use embassy_rp::usb::{Driver, InterruptHandler};
// use embassy_stm32::peripherals::{PA1, PA2, PA3, PA4, PA5, PA6, PA7};
// use embassy_stm32::usb_otg::Driver;

const VENDOR_STRING: &'static str = "TEST";
const PRODUCT_STRING: &'static str = "TEST CON";
const SERIAL_NUMBER: &'static str = "157F8F9";

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => InterruptHandler<USB>;
});

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    // let mut config = Config::default();
    //
    // {
    //     use embassy_stm32::rcc::*;
    //     config.rcc.hse = Some(Hse {
    //         freq: Hertz(25_000_000),
    //         mode: HseMode::Oscillator,
    //     });
    //     config.rcc.pll_src = PllSource::HSE;
    //     config.rcc.pll = Some(Pll {
    //         prediv: PllPreDiv::DIV25,
    //         mul: PllMul::MUL336,
    //         divp: Some(PllPDiv::DIV4), // 25mhz / 25 * 336 / 4 = 84Mhz.
    //         divq: Some(PllQDiv::DIV7), // 25mhz / 25 * 336 / 7 = 48Mhz.
    //         divr: None,
    //     });
    //     config.rcc.ahb_pre = AHBPrescaler::DIV1;
    //     config.rcc.apb1_pre = APBPrescaler::DIV2;
    //     config.rcc.apb2_pre = APBPrescaler::DIV1;
    //     config.rcc.sys = Sysclk::PLL1_P;
    // }
    //
    // let mut p = embassy_stm32::init(config);

    // let mut config = Config::new(ClockConfig {
    //     usb_clk: Some(UsbClkConfig {
    //         src: UsbClkSrc::Xosc,
    //         div: 0,
    //         phase: 0,
    //     })
    //    ..Default::default()
    // });

    let p = embassy_rp::init(Default::default());

    info!("STM32 Xinput example");

    // let mut config = usb_otg::Config::default();
    let mut ep_out_buffer = [0u8; 256];


    // config.vbus_detection = false;

    // Create the driver, from the HAL.
    let driver = Driver::new(p.USB, Irqs);

    // Create embassy-usb Config
    let mut config = embassy_usb::Config::new(0x045e, 0x028e);
    config.max_power = 500;
    config.max_packet_size_0 = 8;
    config.device_class = 0xff;
    config.device_sub_class = 0xff;
    config.device_protocol = 0xff;
    config.device_release = 0x0114; // BCDDevice 1.14
    config.supports_remote_wakeup = true;
    config.manufacturer = Some(VENDOR_STRING);
    config.product = Some(PRODUCT_STRING);
    config.serial_number = Some(SERIAL_NUMBER);
    config.self_powered = true;

    // Create embassy-usb DeviceBuilder using the driver and config.
    // It needs some buffers for building the descriptors.
    let mut device_descriptor = [0; 256];
    let mut config_descriptor = [0; 256];
    let mut bos_descriptor = [0; 256];
    let mut control_buf = [0; 64];
    let request_handler = MyRequestHandler {};

    let mut state = XinputState::new();

    // Note: We actually don't need BOS descriptor. It's easy to change. But I'll keep it.
    let mut builder = Builder::new(
        driver,
        config,
        &mut device_descriptor,
        &mut config_descriptor,
        &mut bos_descriptor,
        &mut control_buf,
    );

    // Create classes on the builder.
    let config = crate::xinput::Config {
        vendor_string: Some(VENDOR_STRING),
        product_string: Some(PRODUCT_STRING),
        serial_number_string: Some(SERIAL_NUMBER),
        request_handler: Some(&request_handler),
        ..Default::default()
    };
    let xinput = XinputReaderWriter::<_>::new(&mut builder, &mut state, config);

    // Build the builder.
    let mut usb = builder.build();

    // Run the USB device. Well, here's only the future to run.
    let usb_fut = usb.run();

    // previously I use a single button to test
    // this might be developed to a button for special functions
    // I need abstraction.
    // let mut button = ExtiInput::new(Input::new(p.PA0, Pull::Up), p.EXTI0);

    let mut button = Input::new(p.PIN_24, Pull::Up);

    let (reader, mut writer) = xinput.split();

    // communication between tasks
    let channel = Channel::<NoopRawMutex, (bool, (usize, usize)), 24>::new();
    let sender = channel.sender();
    let receiver = channel.receiver();

    // scan keys and generate key events
    let keypad_fut = async {
        info!("Now waiting for button");

        loop {

            button.wait_for_any_edge().await;

            let state = button.is_low();

            info!("Got button input: {:?}", state);

            sender.send((state, (0, 0))).await;
            Timer::after(Duration::from_hz(120)).await; // also debounce
        }
    };

    // Process key events
    let in_fut = async {
        let mut controller = XinputControlReport::default();

        info!("waiting for endpoint enable");
        writer.ready().await;

        info!("starting key event processing");

        loop {
            let (status, button) = receiver.receive().await;

            info!("Received status");

            let _ = match button {
                (0, 0) => controller.dpad_right = status,
                (1, 0) => controller.dpad_up = status,
                (2, 0) => controller.dpad_left = status,
                (3, 0) => controller.dpad_down = status,
                (0, 1) => controller.button_b = status,
                (1, 1) => controller.button_y = status,
                (2, 1) => controller.button_x = status,
                (3, 1) => controller.button_a = status,
                (0, 2) => controller.button_view = status,
                (1, 2) => controller.button_menu = status,
                (2, 2) => controller.shoulder_left = status,
                (3, 2) => controller.shoulder_right = status,
                _ => {}
            };

            match writer.write_control(&controller).await {
                Ok(()) => {}
                Err(e) => warn!("Failed to send report: {:?}", e),
            };
        }
    };

    // read report from USB host
    // basically rumble and led status
    let out_fut = async {
        reader.run(false, &request_handler).await;
    };

    // Run everything concurrently.
    // If we had made everything `'static` above instead, we could do this using separate tasks instead.
    join4(usb_fut, in_fut, out_fut, keypad_fut).await;
    // join3(usb_fut, in_fut, out_fut).await;
}

struct MyRequestHandler {}

impl RequestHandler for MyRequestHandler {
    fn get_report(&self, id: ReportId, _buf: &mut [u8]) -> Option<usize> {
        info!("Get report for {:?}", id);
        None
    }

    fn set_report(&self, id: ReportId, data: &[u8]) -> OutResponse {
        info!("Set report for {:?}: {=[u8]}", id, data);
        OutResponse::Accepted
    }
}
