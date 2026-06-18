cmake_minimum_required(VERSION 3.20)
project(veyron_sdk CXX)

find_package(Threads REQUIRED)
find_package(gRPC CONFIG REQUIRED)

add_library(veyron_sdk_cpp STATIC
        src/client.cpp
        src/framing.cpp
        include/veyron/client.hpp
        include/veyron/framing.hpp
        include/veyron/plugin.hpp
)


add_library(veyron::sdk ALIAS veyron_sdk_cpp)


target_include_directories(veyron_sdk_cpp
        PUBLIC
        $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
        $<BUILD_INTERFACE:${CMAKE_CURRENT_BINARY_DIR}> # для сгенерированных
        $<INSTALL_INTERFACE:include>
)

target_link_libraries(veyron_sdk_cpp
        PUBLIC
        protobuf::libprotobuf
        gRPC::grpc++
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

add_library(veyron_sdk_lib SHARED
        src/client.cpp
        src/framing.cpp
)

target_include_directories(veyron_sdk_lib
        PUBLIC
        ${CMAKE_CURRENT_SOURCE_DIR}/include
)

target_include_directories(veyron_sdk_lib PRIVATE include)
target_link_libraries(veyron_sdk_lib PRIVATE veyron_proto_lib)