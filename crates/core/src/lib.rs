//! Yakusu (訳す) core: extract, translate, fit and patch Japanese visual
//! novel scripts. The desktop app and the browser dev server are thin shells
//! around [`app::App`].

#![allow(clippy::type_complexity)]

pub mod app;
pub mod config;
pub mod engines;
pub mod error;
pub mod llm;
pub mod pipeline;
pub mod project;
pub mod prompts;
pub mod protect;
pub mod textfit;

pub use error::{Error, Result};
