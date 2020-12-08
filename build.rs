fn main() {
    prost_build::compile_protos(&["src/api.v1.proto"],
                                &["src/"]).unwrap();
}
