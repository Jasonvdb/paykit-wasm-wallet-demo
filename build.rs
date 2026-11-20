//! Gives the engine's `.wasm` a 2 MiB stack.
//!
//! WebAssembly has no guard page: on wasm32-unknown-unknown a Rust stack overflow is not a
//! clean "stack overflow" panic but a trap on an out-of-bounds memory access, which stops
//! the engine with an unhelpful `RuntimeError`. The linker's default for a wasm32 `cdylib` is 1 MiB. paykit-rs measured the Paykit pay
//! flow at about 192 KiB of stack in a release build and about 1.2 MiB in a debug build
//! (paykit-browser's README, "Stack"). 2 MiB covers both with room to spare, as paykit-rs's
//! own browser harness does. A smaller stack would not make the module smaller: the stack is
//! zeroed memory, not bytes in the file.

/// The stack size of the engine's `.wasm`, in bytes.
const STACK_BYTES: u32 = 2 * 1024 * 1024;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        println!("cargo:rustc-link-arg-cdylib=-zstack-size={STACK_BYTES}");
    }
}
