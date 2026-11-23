//! Code naming the engine's crates and the rest of the simulator. Rejected
//! in the reference model, accepted elsewhere in the simulator.

mod granite_storage {
    pub fn open() {}
}
mod granite_lsm {
    pub fn get() {}
}
mod granite_columnar {
    pub fn read() {}
}
mod granite_shard {
    pub fn apply() {}
}

use granite_storage::open;

pub fn engine() {
    open();
    granite_lsm::get();
    crate::granite_columnar::read();
    self::granite_shard::apply();
}
