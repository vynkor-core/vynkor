cmake_minimum_required(VERSION 3.20)
project(veyron_proto CXX)

find_package(Protobuf REQUIRED)

protobuf_generate_cpp(PROTO_SRCS PROTO_HDRS veyron_protocol.proto)

add_library(veyron_proto_lib STATIC
        ${PROTO_SRCS}
        ${PROTO_HDRS}
)


target_include_directories(veyron_proto_lib PUBLIC ${CMAKE_CURRENT_BINARY_DIR})
target_include_directories(veyron_proto_lib PUBLIC ${PROTOBUF_INCLUDE_DIRS})
target_link_libraries(veyron_proto_lib PUBLIC ${PROTOBUF_LIBRARIES})

