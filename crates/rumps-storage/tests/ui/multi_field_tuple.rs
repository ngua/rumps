use rumps_storage::orm::ToRumps;

#[derive(ToRumps)]
#[rumps(global = "test")]
struct MultiFieldTuple(u64, String);

fn main() {}
