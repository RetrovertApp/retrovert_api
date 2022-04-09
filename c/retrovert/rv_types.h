#pragma once

#include <stdint.h>

#ifndef rv_sizeof_array
#define rv_sizeof_array(x) sizeof(x) / sizeof(x[0])
#endif
