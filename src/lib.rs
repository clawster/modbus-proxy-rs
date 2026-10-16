//! Modbus TCP proxy: lets multiple clients share one modbus device.
//!
//! Data flow per configured device:
//! client socket -> [`bridge`] -> `Message` channel -> [`device`] -> modbus socket

#[macro_use]
extern crate log;

mod bridge;
mod device;
mod frame;
mod ipfilter;
mod modbus;
mod registers;
mod server;
mod settings;
mod stats;
#[cfg(test)]
mod test_util;
mod timeouts;

pub use server::Server;

type Error = Box<dyn std::error::Error + Send + Sync + 'static>;
type Result<T> = std::result::Result<T, Error>;

// Use Jemalloc only for musl-64 bits platforms
//#[cfg(all(target_env = "musl", target_pointer_width = "64"))]
//#[global_allocator]
//static ALLOC: jemallocator::Jemalloc = jemallocator::Jemalloc;
