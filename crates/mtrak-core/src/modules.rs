//! Fennel module source map shared by the model, I/O, and TUI layers.
//!
//! Project storage keeps every authored module in one ID-keyed map: module
//! `00` (the editable project/library module) plus the user modules in
//! `40`–`7F`. The built-in modules `01`–`3F` are embedded in the engine and are
//! never serialized.

use std::collections::BTreeMap;

/// The minimal valid module template used for module `00` and for lazily
/// created user modules. It exposes the required `run` entry point while
/// returning a module table for `require`.
pub const MODULE_TEMPLATE: &str = "(local M {})\n\n(fn M.run [arg1 arg2 ctx]\n  nil)\n\nM\n";

/// The embedded, read-only built-in module sources (`01`–`3F`), keyed by ID.
///
/// The Fennel sources live in `src/builtins/NN.fnl` and are compiled into the
/// executable. They are inspectable in the Code screen but never serialized.
pub const BUILTIN_MODULES: &[(u8, &str)] = &[
    (0x01, include_str!("builtins/01.fnl")),
    (0x02, include_str!("builtins/02.fnl")),
];

/// Looks up an embedded built-in module source.
pub fn builtin_module(id: u8) -> Option<&'static str> {
    BUILTIN_MODULES
        .iter()
        .find(|(builtin, _)| *builtin == id)
        .map(|(_, source)| *source)
}

/// The setup/library module ID.
pub const SETUP: u8 = 0x00;

/// Whether `id` is the setup/library module.
pub fn is_setup(id: u8) -> bool {
    id == SETUP
}

/// Whether `id` is a user-authorable module (`40`–`7F`).
pub fn is_user_module(id: u8) -> bool {
    (0x40..=0x7F).contains(&id)
}

/// Whether `id` is in the built-in module range (`01`–`3F`).
pub fn is_builtin_module(id: u8) -> bool {
    (0x01..=0x3F).contains(&id)
}

/// Whether `id` is reserved or unavailable (`80`–`FF`).
pub fn is_reserved_module(id: u8) -> bool {
    id >= 0x80
}

/// The modules present in a brand-new project: just module `00`.
pub fn default_modules() -> BTreeMap<u8, String> {
    let mut modules = BTreeMap::new();
    modules.insert(SETUP, MODULE_TEMPLATE.to_string());
    modules
}
