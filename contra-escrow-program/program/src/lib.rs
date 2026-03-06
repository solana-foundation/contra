#![no_std]
// ci: test coverage table
pub mod constants;
pub mod error;
pub mod events;
pub mod instructions;
pub mod processor;
pub mod state;

#[cfg(not(feature = "no-entrypoint"))]
pub mod entrypoint;

use pinocchio::address::declare_id;
declare_id!("GokvZqD2yP696rzNBNbQvcZ4VsLW7jNvFXU1kW9m7k83");
