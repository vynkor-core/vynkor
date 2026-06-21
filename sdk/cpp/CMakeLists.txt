cmake_minimum_required(VERSION 3.20)
project(veyron_sdk CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

find_package(Threads REQUIRED)
find_package(Protobuf REQUIRED)

# Generate C++ bindings from the shared proto
set(PROTO_FILE ${CMAKE_CURRENT_SOURCE_DIR}/../../proto/veyron_protocol.proto)
protobuf_generate_cpp(PROTO_SRCS PROTO_HDRS ${PROTO_FILE})

add_library(veyron_sdk_cpp STATIC
    src/client.cpp
    src/framing.cpp
    include/veyron/client.hpp
    include/veyron/framing.hpp
    include/veyron/plugin.hpp
    ${PROTO_SRCS}
    ${PROTO_HDRS}
)

add_library(veyron::sdk ALIAS veyron_sdk_cpp)

target_include_directories(veyron_sdk_cpp
    PUBLIC
    $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
    $<BUILD_INTERFACE:${CMAKE_CURRENT_BINARY_DIR}>
    $<INSTALL_INTERFACE:include>
)

target_link_libraries(veyron_sdk_cpp
    PUBLIC
    protobuf::libprotobuf
    Threads::Threads
)

include(GNUInstallDirs)

install(
    TARGETS veyron_sdk_cpp
    EXPORT veyron-sdk-targets
    ARCHIVE  DESTINATION ${CMAKE_INSTALL_LIBDIR}
    INCLUDES DESTINATION ${CMAKE_INSTALL_INCLUDEDIR}
)

install(
    DIRECTORY include/
    DESTINATION ${CMAKE_INSTALL_INCLUDEDIR}
    FILES_MATCHING PATTERN "*.hpp"
)

install(
    EXPORT veyron-sdk-targets
    FILE veyron-sdk-config.cmake
    NAMESPACE veyron::
    DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/veyron-sdk
)
