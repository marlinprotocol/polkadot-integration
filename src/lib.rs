pub mod node_keys;

pub mod api {
    include!(concat!(env!("OUT_DIR"), "/api.v1.rs"));
}
