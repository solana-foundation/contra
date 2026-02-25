#![no_std]

pub mod constants;
pub mod discriminator;
pub mod error;
pub mod events;
#[cfg(feature = "idl")]
pub mod instructions;
pub mod processor;

#[cfg(not(feature = "no-entrypoint"))]
pub mod entrypoint;

pinocchio_pubkey::declare_id!("J231K9UEpS4y4KAPwGc4gsMNCjKFRMYcQBcjVW7vBhVi");
