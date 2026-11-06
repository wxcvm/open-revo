#![allow(dead_code)]
#[path = "../../src-tauri/src/system/vendor_bus/mqtt.rs"]
mod mqtt;

mod hal {
    pub mod vault {
        pub mod xingyao {
            include!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../src-tauri/src/hal/vault/xingyao.rs"
            ));
        }
    }
}
#[path = "../../src-tauri/src/hal/xingyao_protocol.rs"]
mod xingyao_protocol;
