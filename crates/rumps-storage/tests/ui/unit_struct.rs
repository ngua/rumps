use rumps_storage::orm::ToRumps;

#[derive(ToRumps)]
#[rumps(global = "test")]
struct UnitStruct;

fn main() {}
