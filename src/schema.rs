pub mod v1 {
	include!(concat!(env!("OUT_DIR"), "/api.v1.rs"));
	// pub mod finality {
	// 	include!(concat!(env!("OUT_DIR"), "/api.v1.finality.rs"));
	// }
	pub mod light {
		include!(concat!(env!("OUT_DIR"), "/api.v1.light.rs"));
	}
}
