mod array;
mod io;
mod map;
mod math;
mod option;
mod prelude;
mod random;
mod result;
mod string;
mod time;

use super::Environment;

impl Environment {
    /// Register built-in modules.
    pub(super) fn register_builtins(&mut self) {
        self.register_array_builtin();
        self.register_string_builtin();
        self.register_math_builtin();
        self.register_random_builtin();
        self.register_map_builtin();
        self.register_time_builtin();
        self.register_option_builtin();
        self.register_result_builtin();
        self.register_io_builtin();
        self.register_prelude_builtin();
    }
}
