//! defmt implementations for generated C types.

use core::ptr::addr_of;

use defmt::{write, *};

use crate::c;

impl defmt::Format for c::host_rpu_hpqm_info {
    fn format(&self, fmt: Formatter) {
        write!(
            fmt,
            "host_rpu_hpqm_info {{ event_busy_queue: {}, event_avl_queue: {}, cmd_busy_queue: {}, cmd_avl_queue: {}, rx_buf_busy_queue: {} }}",
            self.event_busy_queue,
            self.event_avl_queue,
            self.cmd_busy_queue,
            self.cmd_avl_queue,
            self.rx_buf_busy_queue
        )
    }
}

impl defmt::Format for c::host_rpu_hpq {
    fn format(&self, fmt: Formatter) {
        let enqueue_addr = unsafe { addr_of!((*self).enqueue_addr).read_unaligned() };
        let dequeue_addr = unsafe { addr_of!((*self).dequeue_addr).read_unaligned() };

        write!(
            fmt,
            "host_rpu_hpq {{ enqueue_addr: {}, dequeue_addr: {} }}",
            enqueue_addr, dequeue_addr,
        )
    }
}
