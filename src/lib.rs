#![no_std]
#![deny(unused_must_use)]

mod c_defmt;
mod fw;

use core::cell::{Cell, RefCell};
use core::marker::PhantomData;
use core::mem::{self, align_of, offset_of, size_of, size_of_val, zeroed, ManuallyDrop, MaybeUninit};
use core::{ptr, slice};

use align_data::{include_aligned, Align16};
use defmt::{assert, assert_eq, debug_assert_eq, panic, todo, unwrap, *};
use embassy_net_driver_channel as ch;
use embassy_net_driver_channel::driver::LinkState;
use embassy_time::{Duration, Timer, WithTimeout};
use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal::spi::Operation;
use embedded_hal_async::digital::Wait;
use embedded_hal_async::spi::SpiDevice;
use fw::FirmwareInfo;
use regions::*;

#[allow(unused)]
#[allow(non_camel_case_types)]
#[allow(non_snake_case)]
mod c {
    include!("../fw/bindings.rs");
    pub const RPU_MCU_CORE_INDIRECT_BASE: u32 = 0xC0000000;
    pub const RX_BUF_HEADROOM: u32 = 4;
}

const MTU: usize = 1514;

#[derive(Clone, Copy)]
enum StateInner {
    Done,

    Pending { message: DynMessage },

    Sent(*mut [u8]),
}

struct Shared {
    state: Cell<StateInner>,
    wakers: RefCell<Wakers>,
    /// Whether a scan is in progress.
    scanning: Cell<bool>,
}

struct Wakers {}

pub struct State {
    shared: Shared,
    ch: ch::State<MTU, 4, 4>,
}

impl State {
    pub fn new() -> Self {
        Self {
            ch: ch::State::new(),
            shared: Shared {
                state: Cell::new(StateInner::Done),
                wakers: RefCell::new(Wakers {}),
                scanning: Cell::new(false)
            },
        }
    }
}

pub type NetDriver<'a> = ch::Device<'a, MTU>;

pub async fn new<'a, BUS, IN, OUT>(
    state: &'a mut State,
    bus: BUS,
    bucken: OUT,
    iovdd_ctl: OUT,
    host_irq: IN,
) -> (NetDriver<'a>, Control<'a>, Runner<'a, BUS, IN, OUT>)
where
    BUS: Bus,
    IN: InputPin + Wait,
    OUT: OutputPin,
{
    let (ch_runner, device) = ch::new(&mut state.ch, ch::driver::HardwareAddress::Ethernet([0; 6]));
    let state_ch = ch_runner.state_runner();

    let mut runner = Runner {
        ch: ch_runner,
        state_ch,
        shared: &state.shared,
        bus,
        bucken,
        iovdd_ctl,
        host_irq,
        rpu_info: None,
        num_commands: c::RPU_CMD_START_MAGIC,
    };
    runner.init().await;

    let control = Control {
        shared: &state.shared,
        state_ch,
    };

    (device, control, runner)
}

pub struct Control<'a> {
    shared: &'a Shared,
    state_ch: ch::StateRunner<'a>,
}

impl<'a> Control<'a> {
    pub fn scan<const N: usize>(&mut self, frequencies: [u32; N]) -> Scanner<'a, N> {
        assert!(
            N < c::SCAN_MAX_NUM_FREQUENCIES as usize,
            "Exceeded maximum amount of frequencies to scan: {}",
            c::SCAN_MAX_NUM_FREQUENCIES
        );

        let scan_params = c::scan_params {
            passive_scan: 0x1,
            num_scan_ssids: todo!(),
            scan_ssids: todo!(),
            no_cck: todo!(),
            bands: todo!(),
            ie: todo!(),
            mac_addr: todo!(),
            dwell_time_active: todo!(),
            dwell_time_passive: todo!(),
            num_scan_channels: todo!(),
            skip_local_admin_macs: todo!(),
            center_frequency: ManuallyDrop::new(frequencies),
        };

        let info = c::umac_scan_info {
            scan_reason: c::scan_reason::SCAN_DISPLAY as _,
            scan_params: todo!(),
        };

        let cmd_scan = c::umac_cmd_scan {
            umac_hdr: unsafe { zeroed() },
            info,
        };
    }
}

pub struct Scanner<'a, const N: usize> {
    shared: &'a Shared,
}

trait Command {
    const MESSAGE_TYPE: c::host_rpu_msg_type;

    fn fill(&mut self);
}

macro_rules! impl_cmd {
    (sys, $cmd:path, $num:expr) => {
        impl Command for $cmd {
            const MESSAGE_TYPE: c::host_rpu_msg_type = c::host_rpu_msg_type::HOST_RPU_MSG_TYPE_SYSTEM;
            fn fill(&mut self) {
                self.sys_head = c::sys_head {
                    cmd_event: $num as _,
                    len: size_of::<Self>() as _,
                };
            }
        }

        impl_cmd!(@common, $cmd);
    };
    (umac, $cmd:path, $num:expr) => {
        impl Command for $cmd {
            const MESSAGE_TYPE: c::host_rpu_msg_type = c::host_rpu_msg_type::HOST_RPU_MSG_TYPE_UMAC;
            fn fill(&mut self) {
                self.umac_hdr = c::umac_hdr {
                    cmd_evnt: $num as _,
                    ..unsafe { zeroed() }
                };
            }
        }

        impl_cmd!(@common, $cmd);
    };

    (@common, $cmd: path) => {
        impl $cmd {
            pub const SIZE: usize = mem::size_of::<Self>();

            #[allow(unused)]
            pub fn to_bytes(&self) -> [u8; Self::SIZE] {
                unsafe { core::ptr::read(core::mem::transmute(self)) }
            }

            #[allow(unused)]
            pub fn from_bytes(bytes: &[u8; Self::SIZE]) -> &Self {
                let alignment = core::mem::align_of::<Self>();
                assert_eq!(
                    bytes.as_ptr().align_offset(alignment),
                    0,
                    "{} is not aligned",
                    core::any::type_name::<Self>()
                );
                unsafe { core::mem::transmute(bytes) }
            }

            #[allow(unused)]
            pub fn from_bytes_mut(bytes: &mut [u8; Self::SIZE]) -> &mut Self {
                let alignment = core::mem::align_of::<Self>();
                assert_eq!(
                    bytes.as_ptr().align_offset(alignment),
                    0,
                    "{} is not aligned",
                    core::any::type_name::<Self>()
                );

                unsafe { core::mem::transmute(bytes) }
            }
        }
    }
}

impl_cmd!(sys, c::cmd_sys_init, c::sys_commands::CMD_INIT);
impl_cmd!(
    umac,
    c::umac_cmd_change_macaddr,
    c::umac_commands::UMAC_CMD_CHANGE_MACADDR
);
impl_cmd!(umac, c::umac_cmd_chg_vif_state, c::umac_commands::UMAC_CMD_SET_IFFLAGS);

impl_cmd!(umac, c::umac_cmd_scan, c::umac_commands::UMAC_CMD_TRIGGER_SCAN);
impl_cmd!(umac, c::umac_cmd_abort_scan, c::umac_commands::UMAC_CMD_ABORT_SCAN);
impl_cmd!(
    umac,
    c::umac_cmd_get_scan_results,
    c::umac_commands::UMAC_CMD_GET_SCAN_RESULTS
);

fn sliceit<T>(t: &T) -> &[u8] {
    unsafe { slice::from_raw_parts(t as *const _ as _, size_of::<T>()) }
}

fn unsliceit2<T>(t: &[u8]) -> (&T, &[u8]) {
    assert!(t.len() > size_of::<T>());
    assert!(t.as_ptr() as usize % align_of::<T>() == 0);
    (unsafe { &*(t.as_ptr() as *const T) }, &t[size_of::<T>()..])
}

fn unsliceit<T>(t: &[u8]) -> &T {
    unsliceit2(t).0
}

fn meh<T>(t: T) -> T {
    t
}

fn slice8(x: &[u32]) -> &[u8] {
    let len = x.len() * 4;
    unsafe { slice::from_raw_parts(x.as_ptr() as _, len) }
}

fn slice8_mut(x: &mut [u32]) -> &mut [u8] {
    let len = x.len() * 4;
    unsafe { slice::from_raw_parts_mut(x.as_mut_ptr() as _, len) }
}

fn slice32(x: &[u8]) -> &[u32] {
    assert!(x.len() % 4 == 0);
    assert!(x.as_ptr() as usize % 4 == 0);
    let len = x.len() / 4;
    unsafe { slice::from_raw_parts(x.as_ptr() as _, len) }
}

fn slice32_mut(x: &mut [u8]) -> &mut [u32] {
    assert!(x.len() % 4 == 0);
    assert!(x.as_ptr() as usize % 4 == 0);
    let len = x.len() / 4;
    unsafe { slice::from_raw_parts_mut(x.as_ptr() as _, len) }
}

#[derive(Copy, Clone, Debug, defmt::Format)]
struct MemoryRegion {
    start: u32,
    end: u32,

    /// Number of dummy 32bit words
    latency: u32,

    rpu_mem_start: u32,
    rpu_mem_end: u32,
    processor_restriction: Option<Processor>,
}

#[rustfmt::skip]
pub(crate) mod regions {
    use super::*;
	pub(crate) const SYSBUS       : &MemoryRegion = &MemoryRegion { start: 0x000000, end: 0x008FFF, latency: 1, rpu_mem_start: 0xA4000000, rpu_mem_end: 0xA4FFFFFF, processor_restriction: None };
	pub(crate) const EXT_SYS_BUS  : &MemoryRegion = &MemoryRegion { start: 0x009000, end: 0x03FFFF, latency: 2, rpu_mem_start: 0,          rpu_mem_end: 0,          processor_restriction: None };
	pub(crate) const PBUS         : &MemoryRegion = &MemoryRegion { start: 0x040000, end: 0x07FFFF, latency: 1, rpu_mem_start: 0xA5000000, rpu_mem_end: 0xA5FFFFFF, processor_restriction: None };
	pub(crate) const PKTRAM       : &MemoryRegion = &MemoryRegion { start: 0x0C0000, end: 0x0F0FFF, latency: 0, rpu_mem_start: 0xB0000000, rpu_mem_end: 0xB0FFFFFF, processor_restriction: None };
	pub(crate) const GRAM         : &MemoryRegion = &MemoryRegion { start: 0x080000, end: 0x092000, latency: 1, rpu_mem_start: 0xB7000000, rpu_mem_end: 0xB7FFFFFF, processor_restriction: None };
	pub(crate) const LMAC_ROM     : &MemoryRegion = &MemoryRegion { start: 0x100000, end: 0x134000, latency: 1, rpu_mem_start: 0x80000000, rpu_mem_end: 0x80033FFF, processor_restriction: Some(Processor::LMAC) }; // ROM
	pub(crate) const LMAC_RET_RAM : &MemoryRegion = &MemoryRegion { start: 0x140000, end: 0x14C000, latency: 1, rpu_mem_start: 0x80040000, rpu_mem_end: 0x8004BFFF, processor_restriction: Some(Processor::LMAC) }; // retained RAM
	pub(crate) const LMAC_SRC_RAM : &MemoryRegion = &MemoryRegion { start: 0x180000, end: 0x190000, latency: 1, rpu_mem_start: 0x80080000, rpu_mem_end: 0x8008FFFF, processor_restriction: Some(Processor::LMAC) }; // scratch RAM
	pub(crate) const UMAC_ROM     : &MemoryRegion = &MemoryRegion { start: 0x200000, end: 0x261800, latency: 1, rpu_mem_start: 0x80000000, rpu_mem_end: 0x800617FF, processor_restriction: Some(Processor::UMAC) }; // ROM
	pub(crate) const UMAC_RET_RAM : &MemoryRegion = &MemoryRegion { start: 0x280000, end: 0x2A4000, latency: 1, rpu_mem_start: 0x80080000, rpu_mem_end: 0x800A3FFF, processor_restriction: Some(Processor::UMAC) }; // retained RAM
	pub(crate) const UMAC_SRC_RAM : &MemoryRegion = &MemoryRegion { start: 0x300000, end: 0x338000, latency: 1, rpu_mem_start: 0x80100000, rpu_mem_end: 0x80137FFF, processor_restriction: Some(Processor::UMAC) }; // scratch RAM

    pub(crate) const REGIONS: [&MemoryRegion; 11] = [
        SYSBUS, EXT_SYS_BUS, PBUS, PKTRAM, GRAM, LMAC_ROM, LMAC_RET_RAM, LMAC_SRC_RAM, UMAC_ROM, UMAC_RET_RAM, UMAC_SRC_RAM
    ];

    #[doc(alias = "pal_rpu_addr_offset_get")]
    pub(crate) fn remap_global_addr_to_region_and_offset(rpu_addr: u32, processor: Option<Processor>) -> (&'static MemoryRegion, u32) {
        defmt::unwrap!(
            REGIONS
                .into_iter()
                .filter(|region| region.processor_restriction.is_none() || region.processor_restriction == processor)
                .find(|region| rpu_addr >= region.rpu_mem_start && rpu_addr <= region.rpu_mem_end)
                .map(|region| (region, rpu_addr - region.rpu_mem_start))
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, defmt::Format)]
pub(crate) enum Processor {
    LMAC,
    UMAC,
}

static FW: &[u8] = include_aligned!(Align16, "../fw/nrf70.bin");

const SR0_WRITE_IN_PROGRESS: u8 = 0x01;

const SR1_RPU_AWAKE: u8 = 0x02;
const SR1_RPU_READY: u8 = 0x04;

const SR2_RPU_WAKEUP_REQ: u8 = 0x01;

const MAX_EVENT_POOL_LEN: usize = 1000;

// ========= config
/*
pktram: 0xB0000000 - 0xB0030FFF -- 196kb
usable for mcu-rpu comms: 0xB0005000 - 0xB0030FFF -- 176kb

First we allocate N tx buffers, which consist of
- Header of 52 bytes
- Data of N bytes

Then we allocate rx buffers.
- 3 queues of
  - N buffers each, which consist of
    - Header of 4 bytes
    - Data of N bytes (default 1600)

Each RX buffer has a "descriptor ID" which is assigned across all queues starting from 0
- queue 0 is descriptors 0..N-1
- queue 1 is descriptors N..2N-1
- queue 2 is descriptors 2N..3N-1
*/

// configurable by user
const MAX_TX_TOKENS: usize = 10;
const MAX_TX_AGGREGATION: usize = 6;
const TX_MAX_DATA_SIZE: usize = 1600;
const RX_MAX_DATA_SIZE: usize = 1600;
const RX_BUFS_PER_QUEUE: usize = 16;

// fixed

const TX_BUFS: usize = MAX_TX_TOKENS * MAX_TX_AGGREGATION;
const TX_BUF_SIZE: usize = c::TX_BUF_HEADROOM as usize + TX_MAX_DATA_SIZE;
const TX_TOTAL_SIZE: usize = TX_BUFS * TX_BUF_SIZE;

const RX_BUFS: usize = RX_BUFS_PER_QUEUE * c::MAX_NUM_OF_RX_QUEUES as usize;
const RX_BUF_SIZE: usize = c::RX_BUF_HEADROOM as usize + RX_MAX_DATA_SIZE;
const RX_TOTAL_SIZE: usize = RX_BUFS * RX_BUF_SIZE;

const _: () = {
    use core::assert;
    assert!(MAX_TX_TOKENS >= 1, "At least one TX token is required");
    assert!(MAX_TX_AGGREGATION <= 16, "Max TX aggregation is 16");
    assert!(RX_BUFS_PER_QUEUE >= 1, "At least one RX buffer per queue is required");
    assert!(
        (TX_TOTAL_SIZE + RX_TOTAL_SIZE) as u32 <= c::RPU_PKTRAM_SIZE,
        "Packet RAM overflow"
    );
};

pub struct Runner<'a, BUS: Bus, IN: InputPin + Wait, OUT: OutputPin> {
    ch: ch::Runner<'a, MTU>,
    state_ch: ch::StateRunner<'a>,
    shared: &'a Shared,

    bus: BUS,
    bucken: OUT,
    iovdd_ctl: OUT,
    host_irq: IN,

    rpu_info: Option<RpuInfo>,

    num_commands: u32,
}

impl<'a, BUS: Bus, IN: InputPin + Wait, OUT: OutputPin> Runner<'a, BUS, IN, OUT> {
    async fn init(&mut self) {
        info!("power on...");
        Timer::after(Duration::from_millis(10)).await;
        self.bucken.set_high().unwrap();
        Timer::after(Duration::from_millis(10)).await;
        self.iovdd_ctl.set_high().unwrap();
        Timer::after(Duration::from_millis(10)).await;

        info!("wakeup...");
        self.rpu_wakeup().await;

        info!("enable clocks...");
        self.raw_write32(PBUS, 0x8C20, 0x0100).await;

        info!("enable interrupt...");
        // First enable the blockwise interrupt for the relevant block in the master register
        let mut val = self.raw_read32(SYSBUS, 0x400).await;
        val |= 1 << 17;
        self.raw_write32(SYSBUS, 0x400, val).await;

        // Now enable the relevant MCU interrupt line
        self.raw_write32(SYSBUS, 0x494, 1 << 31).await;

        let fw_info = fw::FirmwareInfo::read(FW);
        info!("FW info:");

        info!("features: {}", fw_info.features as u32);

        for image in fw_info.images.iter() {
            if let Some(image) = image {
                info!("- Image ty: {}", image.ty as u32);
                info!("- Image data len: {}", image.data.len());
            }
        }

        // UMAC First???
        self.load_patches(&fw_info).await;

        self.boot_lmac(&fw_info).await;
        self.boot_umac(&fw_info).await;

        let lmac_version = self.read32(c::RPU_MEM_LMAC_VER, Some(Processor::LMAC)).await;
        let (version, major, minor, extra) = unpack_version(lmac_version);
        defmt::info!("LMAC version: {}.{}.{}.{}", version, major, minor, extra);

        let umac_version = self.read32(c::RPU_MEM_UMAC_VER, Some(Processor::UMAC)).await;
        let (version, major, minor, extra) = unpack_version(umac_version);
        defmt::info!("UMAC version: {}.{}.{}.{}", version, major, minor, extra);

        // TODO: nrf_wifi_fmac_dev_init_rt

        info!("Initializing rpu info...");
        self.init_rpu_info().await;

        // TODO: OTP info?
        // TODO: RF params
        // TODO: init tx
        info!("Initializing TX...");
        self.init_tx().await;

        // TODO: init rx
        info!("Initializing RX...");
        self.init_rx().await;

        info!("Initializing umac...");
        self.init_umac().await;

        info!("Enabling interrupts...");
        self.rpu_irq_enable().await;

        // TODO: umac_cmd_init
        // TODO: wait for fw init done event

        // For later: setup the interface type
    }

    async fn load_patches(&mut self, fw: &FirmwareInfo<'_>) {
        info!("load UMAC firmware patches...");
        self.load_fw(
            UMAC_RET_RAM,
            c::UMAC_ROM_PATCH_OFFSET,
            fw.get(c::image_ids::IMAGE_UMAC_PRI),
        )
        .await;
        self.load_fw(
            UMAC_RET_RAM,
            c::RPU_MEM_UMAC_PATCH_BIN - UMAC_RET_RAM.rpu_mem_start,
            fw.get(c::image_ids::IMAGE_UMAC_SEC),
        )
        .await;

        info!("load LMAC firmware patches...");
        self.load_fw(
            LMAC_RET_RAM,
            c::LMAC_ROM_PATCH_OFFSET,
            fw.get(c::image_ids::IMAGE_LMAC_PRI),
        )
        .await;
        self.load_fw(
            LMAC_RET_RAM,
            c::RPU_MEM_LMAC_PATCH_BIN - LMAC_RET_RAM.rpu_mem_start,
            fw.get(c::image_ids::IMAGE_LMAC_SEC),
        )
        .await;
    }

    async fn boot_lmac(&mut self, fw: &FirmwareInfo<'_>) {
        info!("booting LMAC");
        // Write firmware signature
        self.write32(c::RPU_MEM_LMAC_BOOT_SIG, Some(Processor::LMAC), fw.signature)
            .await;

        // Write to sleep control register
        self.write32(
            c::RPU_REG_UCC_SLEEP_CTRL_DATA_0,
            Some(Processor::LMAC),
            c::LMAC_ROM_PATCH_OFFSET,
        )
        .await;

        // Write boot vector to RPU
        self.write32(
            c::RPU_REG_MIPS_MCU_BOOT_EXCP_INSTR_0,
            Some(Processor::LMAC),
            c::LMAC_BOOT_EXCP_VECT_0,
        )
        .await;
        self.write32(
            c::RPU_REG_MIPS_MCU_BOOT_EXCP_INSTR_1,
            Some(Processor::LMAC),
            c::LMAC_BOOT_EXCP_VECT_1,
        )
        .await;
        self.write32(
            c::RPU_REG_MIPS_MCU_BOOT_EXCP_INSTR_2,
            Some(Processor::LMAC),
            c::LMAC_BOOT_EXCP_VECT_2,
        )
        .await;
        self.write32(
            c::RPU_REG_MIPS_MCU_BOOT_EXCP_INSTR_3,
            Some(Processor::LMAC),
            c::LMAC_BOOT_EXCP_VECT_3,
        )
        .await;

        // Reset the LMAC
        self.write32(c::RPU_REG_MIPS_MCU_CONTROL, Some(Processor::LMAC), 0x01)
            .await;

        // Wait for the LMAC to boot
        let mut i = 20;

        loop {
            // TODO: Nordic's headers are wrong - LMAC produces 0x5a5a5a5a, not 0xb7000d50 as expected by c::RPU_MEM_LMAC_BOOT_SIG
            if self.read32(c::RPU_MEM_LMAC_BOOT_SIG, None).await == 0x5A5A5A5A {
                break;
            }

            Timer::after_millis(2).await;
            i -= 1;

            if i == 0 {
                panic!("LMAC failed to boot after 40ms");
            }
        }
    }

    async fn boot_umac(&mut self, fw: &FirmwareInfo<'_>) {
        info!("booting UMAC");
        // Write firmware signature
        self.write32(c::RPU_MEM_UMAC_BOOT_SIG, Some(Processor::UMAC), fw.signature)
            .await;

        // Write to sleep control register
        self.write32(
            c::RPU_REG_UCC_SLEEP_CTRL_DATA_1,
            Some(Processor::UMAC),
            c::UMAC_ROM_PATCH_OFFSET,
        )
        .await;

        // Write boot vector to RPU
        self.write32(
            c::RPU_REG_MIPS_MCU2_BOOT_EXCP_INSTR_0,
            Some(Processor::UMAC),
            c::UMAC_BOOT_EXCP_VECT_0,
        )
        .await;
        self.write32(
            c::RPU_REG_MIPS_MCU2_BOOT_EXCP_INSTR_1,
            Some(Processor::UMAC),
            c::UMAC_BOOT_EXCP_VECT_1,
        )
        .await;
        self.write32(
            c::RPU_REG_MIPS_MCU2_BOOT_EXCP_INSTR_2,
            Some(Processor::UMAC),
            c::UMAC_BOOT_EXCP_VECT_2,
        )
        .await;
        self.write32(
            c::RPU_REG_MIPS_MCU2_BOOT_EXCP_INSTR_3,
            Some(Processor::UMAC),
            c::UMAC_BOOT_EXCP_VECT_3,
        )
        .await;

        // Reset the LMAC
        self.write32(c::RPU_REG_MIPS_MCU2_CONTROL, Some(Processor::UMAC), 0x01)
            .await;

        // Wait for the LMAC to boot
        let mut i = 20;

        loop {
            // TODO: Nordic's headers are wrong - LMAC produces 0x5a5a5a5a, not 0xb7000d50 as expected by c::RPU_MEM_LMAC_BOOT_SIG
            if self.read32(c::RPU_MEM_UMAC_BOOT_SIG, None).await == 0x5A5A5A5A {
                break;
            }

            Timer::after_millis(2).await;
            i -= 1;

            if i == 0 {
                panic!("UMAC failed to boot after 40ms");
            }
        }
    }

    pub async fn run(&mut self) -> ! {
        info!("running...");

        let mut buf = [0u32; MAX_EVENT_POOL_LEN / 4];

        loop {
            self.host_irq.wait_for_high().await.unwrap();
            info!("Host IRQ");

            let mut event_count = 0;

            loop {
                let event_address = self
                    .rpu_hpq_dequeue(self.rpu_info.as_ref().unwrap().hpqm_info.event_busy_queue)
                    .await;

                let event_address = match event_address {
                    // No more events to read. Sometimes when low power mode is enabled
                    // we see a wrong address, but it work after a while, so, add a
                    // check for that.
                    None | Some(0xAAAAAAAA) => break,
                    Some(event_address) => event_address,
                };
                event_count += 1;

                self.rpu_event_read(event_address, &mut buf).await;

                let buf = slice8(&buf);
                let (msg, buf) = unsliceit2::<c::host_rpu_msg>(buf);
                match c::host_rpu_msg_type::try_from(msg.type_ as u32) {
                    Ok(c::host_rpu_msg_type::HOST_RPU_MSG_TYPE_SYSTEM) => {
                        let msg: &c::sys_head = unsliceit(buf);
                        match c::sys_events::try_from(msg.cmd_event) {
                            Ok(c::sys_events::EVENT_INIT_DONE) => info!("======== INIT DONE!! =========="),
                            _ => warn!("unknown sys event type {:08x}", meh(msg.cmd_event)),
                        }
                    }
                    _ => warn!("unknown event type {:08x}", meh(msg.type_)),
                }
            }

            if event_count == 0 && self.rpu_irq_watchdog_check().await {
                self.rpu_irq_watchdog_ack().await;
            }

            self.rpu_irq_ack().await;
        }
    }

    async fn rpu_irq_enable(&mut self) {
        // First enable the blockwise interrupt for the relevant block in the master register
        let mut val = self.read32(c::RPU_REG_INT_FROM_RPU_CTRL, None).await;

        val |= 1 << c::RPU_REG_BIT_INT_FROM_RPU_CTRL;

        self.write32(c::RPU_REG_INT_FROM_RPU_CTRL, None, val).await;

        // Now enable the relevant MCU interrupt line
        self.write32(
            c::RPU_REG_INT_FROM_MCU_CTRL,
            None,
            1 << c::RPU_REG_BIT_INT_FROM_MCU_CTRL,
        )
        .await;
    }

    async fn rpu_irq_disable(&mut self) {
        let mut val = self.read32(c::RPU_REG_INT_FROM_RPU_CTRL, None).await;
        val &= !(1 << c::RPU_REG_BIT_INT_FROM_RPU_CTRL);
        self.write32(c::RPU_REG_INT_FROM_RPU_CTRL, None, val).await;

        self.write32(
            c::RPU_REG_INT_FROM_MCU_CTRL,
            None,
            !(1 << c::RPU_REG_BIT_INT_FROM_MCU_CTRL),
        )
        .await;
    }

    async fn rpu_irq_ack(&mut self) {
        // Guess: I think this clears the interrupt flag
        self.write32(c::RPU_REG_INT_FROM_MCU_ACK, None, 1 << c::RPU_REG_BIT_INT_FROM_MCU_ACK)
            .await;
    }

    /// Checks if the watchdog was the source of the interrupt
    async fn rpu_irq_watchdog_check(&mut self) -> bool {
        let val = self.read32(c::RPU_REG_MIPS_MCU_UCCP_INT_STATUS, None).await;
        (val & (1 << c::RPU_REG_BIT_MIPS_WATCHDOG_INT_STATUS)) > 0
    }

    async fn rpu_irq_watchdog_ack(&mut self) {
        info!("ACKing watchdog");
        self.write32(
            c::RPU_REG_MIPS_MCU_UCCP_INT_CLEAR,
            None,
            1 << c::RPU_REG_BIT_MIPS_WATCHDOG_INT_CLEAR,
        )
        .await;
    }

    async fn rpu_event_read(&mut self, event_address: u32, buf: &mut [u32]) {
        self.read(
            event_address,
            None,
            &mut buf[..c::RPU_EVENT_COMMON_SIZE_MAX as usize / 4],
        )
        .await;

        // Get the header from the front of the event data
        let message_header: &c::host_rpu_msg_hdr = unsliceit(slice8(buf));
        if message_header.resubmit > 0 {
            self.rpu_event_free(event_address).await;
        }

        let len = message_header.len as usize;
        if len > MAX_EVENT_POOL_LEN {
            todo!("Fragmented event read is not yet implemented");
        } else if len > c::RPU_EVENT_COMMON_SIZE_MAX as usize {
            // This is a longer than usual event. We gotta read it again
            self.read(event_address, None, &mut buf[..(len + 3) / 4]).await;
        }
    }

    async fn rpu_event_free(&mut self, event_address: u32) {
        self.rpu_hpq_enqueue(self.rpu_info.as_ref().unwrap().hpqm_info.event_avl_queue, event_address)
            .await;
    }

    pub async fn rpu_cmd_ctrl_send(&mut self, message: &[u8]) {
        if message.len() > c::MAX_UMAC_CMD_SIZE as usize {
            todo!("Fragmenting commands is not yet implemented");
        } else {
            // Wait until we get an address to write to
            // This queue might already be full with other messages, so we'll just have to wait a bit
            let message_address = loop {
                if let Some(message_address) = self
                    .rpu_hpq_dequeue(self.rpu_info.as_ref().unwrap().hpqm_info.cmd_avl_queue)
                    .await
                {
                    break message_address;
                }
            };

            // Write the message to the suggested address
            self.write(message_address, None, slice32(message)).await;

            // Post the updated information to the RPU
            self.rpu_hpq_enqueue(
                self.rpu_info.as_ref().unwrap().hpqm_info.cmd_busy_queue,
                message_address,
            )
            .await;

            self.rpu_msg_trigger().await;
        }
    }

    async fn rpu_hpq_enqueue(&mut self, hpq: c::host_rpu_hpq, value: u32) {
        self.write32(hpq.enqueue_addr, None, value).await;
    }

    async fn rpu_hpq_dequeue(&mut self, hpq: c::host_rpu_hpq) -> Option<u32> {
        let value = self.read32(hpq.dequeue_addr, None).await;

        // Pop element only if it is valid
        if value != 0 {
            self.write32(hpq.dequeue_addr, None, value).await;
            Some(value)
        } else {
            None
        }
    }

    async fn init_rpu_info(&mut self) {
        // Based on 'wifi_nrf_hal_dev_init'
        let mut hpqm_info = [0; size_of::<c::host_rpu_hpqm_info>()];
        self.read(c::RPU_MEM_HPQ_INFO, None, slice32_mut(&mut hpqm_info)).await;

        let rx_cmd_base = self.read32(c::RPU_MEM_RX_CMD_BASE, None).await;

        self.rpu_info = Some(RpuInfo {
            hpqm_info: unsafe { mem::transmute_copy(&hpqm_info) },
            rx_cmd_base,
            tx_cmd_base: c::RPU_MEM_TX_CMD_BASE,
        });
    }

    async fn send_cmd<T: Command>(&mut self, mut cmd: T) {
        cmd.fill();

        // FIXME: The guess is wrong, a scan command with 64 frequencies to scan for is 3022 bytes.
        const MAX_CMD_SIZE: usize = 512; // TODO this is a wild guess.

        let mut buf = [0u32; MAX_CMD_SIZE / 4];
        let buf8 = slice8_mut(&mut buf);

        #[repr(C, packed)]
        struct Msg<T> {
            header: c::host_rpu_msg,
            cmd: T,
        }

        let mut cmd = Msg {
            header: unsafe { zeroed() },
            cmd,
        };
        cmd.header.hdr.len = size_of_val(&cmd) as _;
        cmd.header.type_ = T::MESSAGE_TYPE as _;

        let cmd_bytes = sliceit(&cmd);
        buf8[..cmd_bytes.len()].copy_from_slice(cmd_bytes);

        self.rpu_cmd_ctrl_send(slice8(&buf[..(size_of::<T>() + 3) / 4]))
            .with_timeout(Duration::from_secs(1))
            .await
            .expect("timed out")
    }

    async fn init_umac(&mut self) {
        let cmd = c::cmd_sys_init {
            sys_head: unsafe { zeroed() },
            wdev_id: 0,
            sys_params: c::sys_params {
                sleep_enable: 0, // TODO for low power
                hw_bringup_time: c::HW_DELAY,
                sw_bringup_time: c::SW_DELAY,
                bcn_time_out: c::BCN_TIMEOUT,
                calib_sleep_clk: c::CALIB_SLEEP_CLOCK_ENABLE,
                phy_calib: c::DEF_PHY_CALIB,
                mac_addr: [0; 6],
                rf_params: [0; 200],
                rf_params_valid: 0,
            },
            rx_buf_pools: [
                c::rx_buf_pool_params {
                    buf_sz: RX_MAX_DATA_SIZE as _, // TODO is this including the header or not?
                    num_bufs: RX_BUFS_PER_QUEUE as _,
                },
                c::rx_buf_pool_params {
                    buf_sz: RX_MAX_DATA_SIZE as _, // TODO is this including the header or not?
                    num_bufs: RX_BUFS_PER_QUEUE as _,
                },
                c::rx_buf_pool_params {
                    buf_sz: RX_MAX_DATA_SIZE as _, // TODO is this including the header or not?
                    num_bufs: RX_BUFS_PER_QUEUE as _,
                },
            ],
            data_config_params: c::data_config_params {
                rate_protection_type: 0,
                aggregation: 1,
                wmm: 1,
                max_num_tx_agg_sessions: 4,
                max_num_rx_agg_sessions: 8,
                max_tx_aggregation: MAX_TX_AGGREGATION as _,
                reorder_buf_size: 64,
                max_rxampdu_size: 3,
            },
            temp_vbat_config_params: c::temp_vbat_config {
                temp_based_calib_en: c::TEMP_CALIB_ENABLE,
                temp_calib_bitmap: c::DEF_PHY_TEMP_CALIB,
                vbat_calibp_bitmap: c::DEF_PHY_VBAT_CALIB,
                temp_vbat_mon_period: c::TEMP_CALIB_PERIOD,
                vth_very_low: c::VBAT_VERYLOW as _,
                vth_low: c::VBAT_LOW as _,
                vth_hi: c::VBAT_HIGH as _,
                temp_threshold: c::TEMP_CALIB_THRESHOLD as _,
                vbat_threshold: 0,
            },
            country_code: [0, 0],
            mgmt_buff_offload: 0,
            op_band: 0,
            tcp_ip_checksum_offload: 0,
            feature_flags: c::feature_flags::FEAT_SYSTEM_MODE as _,
            disable_beamforming: 1,
            discon_timeout: 0,
            ps_data_retrieval_mech: c::data_retrieve_mechanism::PS_POLL_FRAME as u8,
        };
        self.send_cmd(cmd).await;
    }

    async fn init_tx(&mut self) {}

    async fn init_rx(&mut self) {
        for queue_id in 0..(c::MAX_NUM_OF_RX_QUEUES as usize) {
            for buf_id in 0..RX_BUFS_PER_QUEUE {
                let desc_id = queue_id * RX_BUFS_PER_QUEUE + buf_id;
                let rpu_addr = c::RPU_MEM_PKT_BASE + (TX_TOTAL_SIZE + RX_BUF_SIZE * desc_id) as u32;

                // write rx buffer header
                self.write32(rpu_addr, None, desc_id as u32).await;

                // Create host_rpu_rx_buf_info (it's just one word of the address)
                let command = [rpu_addr + c::RX_BUF_HEADROOM as u32];

                // Call wifi_nrf_hal_data_cmd_send with the command
                self.rpu_rx_cmd_send(&command, desc_id as u32, queue_id).await;
            }
        }
    }

    async fn rpu_rx_cmd_send(&mut self, command: &[u32], desc_id: u32, pool_id: usize) {
        let addr_base = self.rpu_info.as_ref().unwrap().rx_cmd_base;
        let max_cmd_size = c::RPU_DATA_CMD_SIZE_MAX_RX;

        let addr = addr_base + max_cmd_size * desc_id;
        let host_addr = addr & c::RPU_ADDR_MASK_OFFSET | c::RPU_MCU_CORE_INDIRECT_BASE;

        // Write the command to the core
        self.rpu_write_core(host_addr, command, Processor::LMAC).await; // LMAC is a guess here

        // Post the updated information to the RPU
        self.rpu_hpq_enqueue(
            self.rpu_info.as_ref().unwrap().hpqm_info.rx_buf_busy_queue[pool_id],
            addr,
        )
        .await;
    }

    async fn rpu_msg_trigger(&mut self) {
        // Indicate to the RPU that the information has been posted
        self.write32(
            c::RPU_REG_INT_TO_MCU_CTRL,
            Some(Processor::UMAC),
            self.num_commands | 0x7fff0000,
        )
        .await;
        self.num_commands = self.num_commands.wrapping_add(1);
    }

    async fn load_fw(&mut self, mem: &MemoryRegion, addr: u32, fw: &[u8]) {
        const FW_CHUNK_SIZE: usize = 1024;
        for (i, chunk) in fw.chunks(FW_CHUNK_SIZE).enumerate() {
            let offs = addr + (FW_CHUNK_SIZE * i) as u32;
            self.raw_write(mem, offs, slice32(chunk)).await;
        }
    }

    async fn rpu_wait_until_write_done(&mut self) {
        while self.bus.read_sr0().await & SR0_WRITE_IN_PROGRESS != 0 {}
    }

    async fn rpu_wait_until_awake(&mut self) {
        for _ in 0..10 {
            if self.bus.read_sr1().await & SR1_RPU_AWAKE != 0 {
                return;
            }
            Timer::after(Duration::from_millis(1)).await;
        }
        panic!("awakening never came")
    }

    async fn rpu_wait_until_ready(&mut self) {
        for _ in 0..10 {
            if self.bus.read_sr1().await == SR1_RPU_AWAKE | SR1_RPU_READY {
                return;
            }
            Timer::after(Duration::from_millis(1)).await;
        }
        panic!("readyning never came")
    }

    async fn rpu_wait_until_wakeup_req(&mut self) {
        for _ in 0..10 {
            if self.bus.read_sr2().await == SR2_RPU_WAKEUP_REQ {
                return;
            }
            Timer::after(Duration::from_millis(1)).await;
        }
        panic!("wakeup_req never came")
    }

    async fn rpu_wakeup(&mut self) {
        self.bus.write_sr2(SR2_RPU_WAKEUP_REQ).await;
        self.rpu_wait_until_wakeup_req().await;
        self.rpu_wait_until_awake().await;
    }

    async fn rpu_sleep(&mut self) {
        self.bus.write_sr2(0).await;
    }

    async fn rpu_sleep_status(&mut self) -> u8 {
        self.bus.read_sr1().await
    }

    async fn raw_read32_inner(&mut self, mem: &MemoryRegion, offs: u32) -> u32 {
        assert!(mem.start + offs + 4 <= mem.end);
        let lat = mem.latency as usize;

        let mut buf = [0u32; 3];
        self.bus.read(mem.start + offs, &mut buf[..lat + 1]).await;
        buf[lat]
    }

    async fn raw_read32(&mut self, mem: &MemoryRegion, offs: u32) -> u32 {
        let res = self.raw_read32_inner(mem, offs).await;
        info!("read32 {:08x} {:08x}", mem.start + offs, res);
        res
    }

    async fn raw_read(&mut self, mem: &MemoryRegion, offs: u32, buf: &mut [u32]) {
        assert!(mem.start + offs + (buf.len() as u32 * 4) <= mem.end);

        // latency=0 optimization doesn't seem to be working, we read the first word repeatedly.
        if mem.latency == 0 && false {
            // No latency, we can do a big read directly.
            self.bus.read(mem.start + offs, buf).await;
        } else {
            // Otherwise, read word by word.
            for (i, val) in buf.iter_mut().enumerate() {
                *val = self.raw_read32_inner(mem, offs + i as u32 * 4).await;
            }
        }
        info!(
            "read addr={:08x} len={:08x} buf={:02x}",
            mem.start + offs,
            buf.len() * 4,
            slice8(buf)
        );
    }
    async fn raw_write32(&mut self, mem: &MemoryRegion, offs: u32, val: u32) {
        self.raw_write(mem, offs, &[val]).await
    }

    async fn raw_write(&mut self, mem: &MemoryRegion, offs: u32, buf: &[u32]) {
        assert!(mem.start + offs + (buf.len() as u32 * 4) <= mem.end);
        info!(
            "write addr={:08x} len={:08x} buf={:02x}",
            mem.start + offs,
            buf.len() * 4,
            slice8(buf)
        );
        self.bus.write(mem.start + offs, buf).await;
    }

    async fn read32(&mut self, rpu_addr: u32, processor: Option<Processor>) -> u32 {
        let (mem, offs) = regions::remap_global_addr_to_region_and_offset(rpu_addr, processor);
        self.raw_read32(mem, offs).await
    }

    async fn read(&mut self, rpu_addr: u32, processor: Option<Processor>, buf: &mut [u32]) {
        let (mem, offs) = regions::remap_global_addr_to_region_and_offset(rpu_addr, processor);
        self.raw_read(mem, offs, buf).await
    }

    async fn write32(&mut self, rpu_addr: u32, processor: Option<Processor>, val: u32) {
        let (mem, offs) = regions::remap_global_addr_to_region_and_offset(rpu_addr, processor);
        self.raw_write32(mem, offs, val).await
    }

    async fn write(&mut self, rpu_addr: u32, processor: Option<Processor>, buf: &[u32]) {
        let (mem, offs) = regions::remap_global_addr_to_region_and_offset(rpu_addr, processor);
        self.raw_write(mem, offs, buf).await
    }

    async fn rpu_write_core(&mut self, core_address: u32, buf: &[u32], processor: Processor) {
        // We receive the address as a byte address, while we need to write it as a word address
        let addr = (core_address & c::RPU_ADDR_MASK_OFFSET) / 4;

        let (addr_reg, data_reg) = match processor {
            Processor::LMAC => (
                c::RPU_REG_MIPS_MCU_SYS_CORE_MEM_CTRL,
                c::RPU_REG_MIPS_MCU_SYS_CORE_MEM_WDATA,
            ),
            Processor::UMAC => (
                c::RPU_REG_MIPS_MCU2_SYS_CORE_MEM_CTRL,
                c::RPU_REG_MIPS_MCU2_SYS_CORE_MEM_WDATA,
            ),
        };

        // Write the processor address register
        self.write32(addr_reg, Some(processor), addr).await;

        // Write to the data register one by one
        for data in buf {
            self.write32(data_reg, Some(processor), *data).await;
        }
    }
}

pub trait Bus {
    async fn read(&mut self, addr: u32, buf: &mut [u32]);
    async fn write(&mut self, addr: u32, buf: &[u32]);
    async fn read_sr0(&mut self) -> u8;
    async fn read_sr1(&mut self) -> u8;
    async fn read_sr2(&mut self) -> u8;
    async fn write_sr2(&mut self, val: u8);
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

fn unpack_version(version: u32) -> (u32, u32, u32, u32) {
    let v = (version & 0xFF00_0000) >> 24;
    let major = (version & 0x00FF_0000) >> 16;
    let minor = (version & 0x0000_FF00) >> 16;
    let extra = version & 0x0000_00FF;

    (v, major, minor, extra)
}

/*
pub struct QspiBus<'a> {
    qspi: Qspi<'a, QSPI>,
}

impl<'a> QspiBus<'a> {}

impl<'a> Bus for QspiBus<'a> {
    async fn read(&mut self, addr: u32, buf: &mut [u32]) {
        self.qspi.read(addr, slice8_mut(buf)).await.unwrap();
    }

    async fn write(&mut self, addr: u32, buf: &[u32]) {
        self.qspi.write(addr, slice8(buf)).await.unwrap();
    }

    async fn read_sr0(&mut self) -> u8 {
        let mut status = [4; 1];
        unwrap!(self.qspi.custom_instruction(0x05, &[0x00], &mut status).await);
        defmt::trace!("read sr0 = {:02x}", status[0]);
        status[0]
    }

    async fn read_sr1(&mut self) -> u8 {
        let mut status = [4; 1];
        unwrap!(self.qspi.custom_instruction(0x1f, &[0x00], &mut status).await);
        defmt::trace!("read sr1 = {:02x}", status[0]);
        status[0]
    }

    async fn read_sr2(&mut self) -> u8 {
        let mut status = [4; 1];
        unwrap!(self.qspi.custom_instruction(0x2f, &[0x00], &mut status).await);
        defmt::trace!("read sr2 = {:02x}", status[0]);
        status[0]
    }

    async fn write_sr2(&mut self, val: u8) {
        defmt::trace!("write sr2 = {:02x}", val);
        unwrap!(self.qspi.custom_instruction(0x3f, &[val], &mut []).await);
    }
}
 */

/// This structure encapsulates the information which represents a HPQ.
#[repr(C)]
#[derive(Debug, defmt::Format, Clone, Copy)]
pub(crate) struct HostRpuHPQ {
    /// HPQ address where the host can post the address of a
    /// message intended for the RPU.
    enqueue_addr: u32,
    /// HPQ address where the host can get the address of a
    /// message intended for the host.
    dequeue_addr: u32,
}

/// Hostport queue information passed by the RPU to the host, which the host can
/// use, to communicate with the RPU.
#[repr(C)]
#[derive(Debug, defmt::Format, Clone, Copy)]
pub(crate) struct HostRpuHPQMInfo {
    /// Queue which the RPU uses to inform the host about events.
    event_busy_queue: HostRpuHPQ,
    /// Queue on which the consumed events are pushed so that RPU can reuse them.
    event_avl_queue: HostRpuHPQ,
    /// Queue used by the host to push commands to the RPU.
    cmd_busy_queue: HostRpuHPQ,
    /// Queue which RPU uses to inform host about command buffers which can be used to push commands to the RPU.
    cmd_avl_queue: HostRpuHPQ,
    rx_buf_busy_queue: [HostRpuHPQ; c::MAX_NUM_OF_RX_QUEUES as usize],
}

#[derive(Debug, defmt::Format)]
pub(crate) struct RpuInfo {
    hpqm_info: c::host_rpu_hpqm_info,
    /// The base address for posting RX commands.
    rx_cmd_base: u32,
    /// The base address for posting TX commands.
    tx_cmd_base: u32,
}

struct RxPoolMapInfo {
    pool_id: u32,
    buf_id: u32,
}

#[repr(C, packed)]
union MessageInner<Command, Reply> {
    // These fields are fake "Copy"
    command: ManuallyDrop<Command>,
    reply: ManuallyDrop<Reply>,
}

struct Message<Command, Reply> {
    inner: c::host_rpu_msg<MessageInner<Command, Reply>>,
    cmd_len: usize,
    reply_len: usize,
}

impl<Command: crate::Command, Reply> Message<Command, Reply> {
    pub fn new(command: Command) -> Self {
        let cmd_len = mem::size_of::<c::host_rpu_msg>() + mem::size_of::<Command>();
        let reply_len = mem::size_of::<c::host_rpu_msg>() + mem::size_of::<Reply>();
        
        Self {
            inner: c::host_rpu_msg {
                hdr: c::host_rpu_msg_hdr {
                    len: cmd_len as u32,
                    // TODO: Don't know
                    resubmit: 0,
                },
                type_: Command::MESSAGE_TYPE as _,
                msg: ManuallyDrop::new(MessageInner {
                    command: ManuallyDrop::new(command),
                }),
            },
            cmd_len,
            reply_len,
        }
    }

    /// # Safety
    ///
    /// The lifetime of the return value shall not exceed the lifetime of this message.
    pub unsafe fn downgrade(&mut self) -> DynMessage {
        DynMessage {
            ptr: ptr::from_mut(&mut self.inner).cast(),
            cmd_len: self.cmd_len,
            reply_len: self.reply_len,
        }
    }
}

/// A type erased [`Message`]. (Might need to remove the lifetime for sending to the runner, thereby making access unsafe)
#[derive(Clone, Copy)]
struct DynMessage {
    ptr: *mut c::host_rpu_msg,
    cmd_len: usize,
    reply_len: usize,
}

impl DynMessage {
    pub fn ty(&self) -> c::host_rpu_msg_type {
        let msg = unsafe { self.ptr.read_unaligned() };
        unsafe { mem::transmute(msg.type_) }
    }

    /// Get the content of this message as a slice for sending to the device.
    ///
    /// # Safety
    ///
    /// - The memory referenced by the returned slice must not be mutated for the duration of lifetime 'a.
    ///   'a must also be shorter than the lifetime of the message.
    /// - The returned slice is only valid before accessing the reply slice.
    pub unsafe fn as_cmd_slice<'a>(&self) -> &'a [u8] {
        // SAFETY: TODO
        let slice = unsafe { slice::from_raw_parts(self.ptr.cast::<u8>(), self.cmd_len) };
        slice
    }

    /// Get the content of this message as a slice for sending to the device.
    ///
    /// # Safety
    ///
    /// - The memory referenced by the returned slice must not be accessed through
    ///   any other pointer (not derived from the return value) for the duration of
    ///   lifetime 'a. Both read and write accesses are forbidden. 'a must also be shorter
    ///   than the lifetime of the message.
    /// - Writing to this slice invalidates the command view of the slice.
    pub unsafe fn as_reply_slice<'a>(&mut self) -> &'a mut [u8] {
        let slice = unsafe { slice::from_raw_parts_mut(self.ptr.cast::<u8>(), self.reply_len) };
        slice
    }
}
