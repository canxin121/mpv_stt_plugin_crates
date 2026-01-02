# Generic Android toolchain wrapper for cmake-based deps (e.g. whisper.cpp).
# Expects ANDROID_NDK and ANDROID_ABI in the environment.

if(NOT DEFINED ANDROID_NDK)
  if(DEFINED ENV{ANDROID_NDK_HOME})
    set(ANDROID_NDK "$ENV{ANDROID_NDK_HOME}")
  elseif(DEFINED ENV{NDK})
    set(ANDROID_NDK "$ENV{NDK}")
  elseif(DEFINED ENV{CMAKE_ANDROID_NDK})
    set(ANDROID_NDK "$ENV{CMAKE_ANDROID_NDK}")
  else()
    message(FATAL_ERROR "ANDROID_NDK not set. Set ANDROID_NDK_HOME or NDK.")
  endif()
endif()

if(NOT DEFINED ANDROID_API)
  if(DEFINED ENV{ANDROID_API})
    set(ANDROID_API "$ENV{ANDROID_API}")
  elseif(DEFINED ENV{API})
    set(ANDROID_API "$ENV{API}")
  else()
    set(ANDROID_API 21)
  endif()
endif()

if(NOT DEFINED ANDROID_ABI)
  if(DEFINED ENV{ANDROID_ABI})
    set(ANDROID_ABI "$ENV{ANDROID_ABI}")
  else()
    message(FATAL_ERROR "ANDROID_ABI not set. Set ANDROID_ABI (arm64-v8a, armeabi-v7a, x86, x86_64).")
  endif()
endif()

set(ANDROID_PLATFORM android-${ANDROID_API})
set(CMAKE_ANDROID_NDK "${ANDROID_NDK}")
set(CMAKE_ANDROID_STL_TYPE c++_shared)

include("${ANDROID_NDK}/build/cmake/android.toolchain.cmake")
