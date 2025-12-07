use rumps_storage::orm::{FromRumps, ToRumps};

#[derive(ToRumps, FromRumps)]
#[rumps(global = "test")]
struct NoKeyField {
    name: String,
    value: u32,
}

fn main() {}
