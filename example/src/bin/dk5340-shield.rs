//! This uses the nRF7002-EK shield with a nRF5340-DK.

#![no_std]
#![no_main]
#![deny(unused_must_use)]

use defmt::*;
use defmt_rtt as _; // global logger
use embassy_executor::Spawner;
use embassy_nrf::{bind_interrupts, gpio::{AnyPin, Input, Level, Output, OutputDrive, Pin, Pull}, spim::{self, Spim}};
use embassy_time::{Delay, Duration, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;
use nrf70::SpiBus;
use {embassy_nrf as _, panic_probe as _};

bind_interrupts!(struct Irqs {
    SERIAL0 => spim::InterruptHandler<embassy_nrf::peripherals::SERIAL0>;
});

#[embassy_executor::task]
async fn blink_task(led: AnyPin) -> ! {
    let mut led = Output::new(led, Level::High, OutputDrive::Standard);
    loop {
        led.set_high();
        Timer::after(Duration::from_millis(250)).await;
        led.set_low();
        Timer::after(Duration::from_millis(250)).await;
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    info!("Hello World!");
    let config: embassy_nrf::config::Config = Default::default();
    let p = embassy_nrf::init(config);
    spawner.spawn(blink_task(p.P0_28.degrade())).unwrap();

    // TODO: the 5340dk datasheet says you should only use P0.13-P0.18 for QSPI, but these are allowed in the hal?
    let sck = p.P1_15;
    let csn = p.P1_12;
    let dio0 = p.P1_13;
    let dio1 = p.P1_14;
    let _dio2 = p.P1_10;
    let _dio3 = p.P1_11;

    // TODO: COEX pins
    let bucken = Output::new(p.P1_00, Level::Low, OutputDrive::HighDrive);
    let iovdd_ctl = Output::new(p.P1_01.degrade(), Level::Low, OutputDrive::Standard);
    let host_irq = Input::new(p.P1_09.degrade(), Pull::None);

    // TODO: QSPI

    let mut config = spim::Config::default();
    config.frequency = spim::Frequency::M8;
    let spim = Spim::new(p.SERIAL0, Irqs, sck, dio1, dio0, config);
    let csn = Output::new(csn, Level::High, OutputDrive::HighDrive);
    let spi = ExclusiveDevice::new(spim, csn, Delay);
    let bus = SpiBus::new(spi);

    let mut state = nrf70::State::new();
    let (device, control, mut runner) = nrf70::new(&mut state, bus, bucken, iovdd_ctl, host_irq).await;

    runner.run().await;
}
