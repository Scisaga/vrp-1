//! 由 `proto/` 生成的契约类型。模块路径与 proto 包名一致（去掉顶层的 `vrp`）。
// pbjson-build 0.9 emits `write!(..., &FIELDS)` in generated visitors.
#![allow(clippy::useless_borrows_in_formatting)]

pub mod maps {
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/vrp.maps.v1.rs"));
        include!(concat!(env!("OUT_DIR"), "/vrp.maps.v1.serde.rs"));
    }
}

pub mod engine {
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/vrp.engine.v1.rs"));
        include!(concat!(env!("OUT_DIR"), "/vrp.engine.v1.serde.rs"));
    }
}

pub mod worker {
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/vrp.worker.v1.rs"));
        include!(concat!(env!("OUT_DIR"), "/vrp.worker.v1.serde.rs"));
    }
}

pub mod app {
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/vrp.app.v1.rs"));
        include!(concat!(env!("OUT_DIR"), "/vrp.app.v1.serde.rs"));
    }
}
