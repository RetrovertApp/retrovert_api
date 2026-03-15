# PlaybackPlugin.cmake
#
# Common setup for Retrovert playback plugins.
# Include this from a plugin's CMakeLists.txt to get sensible defaults.
#
# Sets:
#   RETROVERT_INCLUDE_DIR     - path to vendored API headers
#   RETROVERT_PLUGIN_OUTPUT_DIR - where built plugins go
#   PLAYBACK_PLUGIN_SUFFIX    - .so or .dll
#
# Also includes DependencyHelpers.cmake (download_library, suppress_external_warnings).

include_guard()

# Include dependency helpers from the same cmake/ directory
include(DependencyHelpers)

# API headers — vendored in each plugin repo
if(NOT RETROVERT_INCLUDE_DIR)
    set(RETROVERT_INCLUDE_DIR "${CMAKE_CURRENT_SOURCE_DIR}/include")
endif()

# Output directory for built plugin shared libraries
if(NOT RETROVERT_PLUGIN_OUTPUT_DIR)
    set(RETROVERT_PLUGIN_OUTPUT_DIR "${CMAKE_BINARY_DIR}/plugins")
endif()

# Plugin file suffix
if(NOT PLAYBACK_PLUGIN_SUFFIX)
    if(WIN32)
        set(PLAYBACK_PLUGIN_SUFFIX ".dll")
    else()
        set(PLAYBACK_PLUGIN_SUFFIX ".so")
    endif()
endif()
