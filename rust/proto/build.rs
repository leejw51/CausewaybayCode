fn main() {
    println!("cargo:rerun-if-changed=schema/cwbcode.capnp");
    capnpc::CompilerCommand::new().src_prefix("schema").file("schema/cwbcode.capnp").run().expect("compiling schema/cwbcode.capnp (is the `capnp` tool installed? brew install capnp)");
}
