use core::future::Future;

use embedded_hal::spi::Operation;
use embedded_hal_async::spi::SpiDevice;

use crate::{slice8, slice8_mut};

pub trait Bus {
    fn read(&mut self, addr: u32, buf: &mut [u32]) -> impl Future<Output = ()>;
    fn write(&mut self, addr: u32, buf: &[u32]) -> impl Future<Output = ()>;
    fn read_sr0(&mut self) -> impl Future<Output = u8>;
    fn read_sr1(&mut self) -> impl Future<Output = u8>;
    fn read_sr2(&mut self) -> impl Future<Output = u8>;
    fn write_sr2(&mut self, val: u8) -> impl Future<Output = ()>;
}

pub struct SpiBus<T> {
    spi: T,
}

impl<T> SpiBus<T> {
    pub fn new(spi: T) -> Self {
        Self { spi }
    }
}

impl<T: SpiDevice> Bus for SpiBus<T> {
    async fn read(&mut self, addr: u32, buf: &mut [u32]) {
        self.spi
            .transaction(&mut [
                Operation::Write(&[0x0B, (addr >> 16) as u8, (addr >> 8) as u8, addr as u8, 0x00]),
                Operation::Read(slice8_mut(buf)),
            ])
            .await
            .unwrap()
    }

    async fn write(&mut self, addr: u32, buf: &[u32]) {
        self.spi
            .transaction(&mut [
                Operation::Write(&[0x02, (addr >> 16) as u8 | 0x80, (addr >> 8) as u8, addr as u8]),
                Operation::Write(slice8(buf)),
            ])
            .await
            .unwrap()
    }

    async fn read_sr0(&mut self) -> u8 {
        let mut buf = [0; 2];
        self.spi.transfer(&mut buf, &[0x05]).await.unwrap();
        let val = buf[1];
        defmt::trace!("read sr0 = {:02x}", val);
        val
    }

    async fn read_sr1(&mut self) -> u8 {
        let mut buf = [0; 2];
        self.spi.transfer(&mut buf, &[0x1f]).await.unwrap();
        let val = buf[1];
        defmt::trace!("read sr1 = {:02x}", val);
        val
    }

    async fn read_sr2(&mut self) -> u8 {
        let mut buf = [0; 2];
        self.spi.transfer(&mut buf, &[0x2f]).await.unwrap();
        let val = buf[1];
        defmt::trace!("read sr2 = {:02x}", val);
        val
    }

    async fn write_sr2(&mut self, val: u8) {
        defmt::trace!("write sr2 = {:02x}", val);
        self.spi.write(&[0x3f, val]).await.unwrap();
    }
}
