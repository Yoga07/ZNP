extern crate chrono;
extern crate hex;
extern crate rand;
extern crate rocksdb;
extern crate rug;
extern crate sha3;
extern crate sodiumoxide;

mod comms_handler;
mod compute;
mod constants;
mod db;
mod interfaces;
mod key_creation;
mod miner;
mod primitives;
mod script;
mod storage;
#[cfg(test)]
mod test_utils;
#[cfg(test)]
mod tests;
mod unicorn;
mod user;
mod utils;

#[cfg(not(features = "mock"))]
pub(crate) use comms_handler::Node;
#[cfg(features = "mock")]
pub(crate) use mock::Node;

fn main() {}
