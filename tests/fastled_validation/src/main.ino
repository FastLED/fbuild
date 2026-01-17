// FastLED Validation sketch - bridges fbuild to FastLED validation tests
// This file includes the FastLED Validation.ino sketch directly so fbuild can compile it

// The Validation sketch is located at:
// ~/dev/fastled9/examples/Validation/Validation.ino

// Include the validation sketch using the path from build_flags -I directive
// The platformio.ini has: -I../../../fastled9/examples/Validation
#include "Validation.ino"
