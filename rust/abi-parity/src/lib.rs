//! Layout parity gate: `retrovert-host`'s hand-written mirror against bindgen output
//! generated from `include/retrovert/` at build time.

#[cfg(test)]
mod c {
    #![allow(
        non_camel_case_types,
        non_snake_case,
        non_upper_case_globals,
        dead_code
    )]

    include!(concat!(env!("OUT_DIR"), "/rv_abi.rs"));
}

#[cfg(test)]
mod parity;
