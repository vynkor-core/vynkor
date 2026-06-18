cmake_minimum_required(VERSION 3.20)
project(veyron
    VERSION 0.1.0
    DESCRIPTION "Veyron — plugin runtime core"
    LANGUAGES CXX
)

# C++23
set(CMAKE_CXX_STANDARD 23)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

set(CMAKE_CXX_VISIBILITY_PRESET hidden)
set(CMAKE_VISIBILITY_INLINES_HIDDEN ON)

find_package(Threads REQUIRED)

# SDK
add_subdirectory(sdk/cpp)
# protobuf
add_subdirectory(proto)