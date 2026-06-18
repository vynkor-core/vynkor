#include <cstdint>
#include <string>
#include <vector>
#include "proto/veyron_protocol.pb.h"
#if defined(_MSC_VER)
#define PACKED_STRUCT(name) __pragma(pack(push, 1)) struct name __pragma(pack(pop))
#else
#define PACKED_STRUCT(name) struct [[gnu::packed]] name
#endif

namespace veyron {
    class client {
        PACKED_STRUCT(VeyronHeader) {
            uint8_t magic;
            uint8_t flags;
            uint32_t length;
            char target[32];
            uint16_t crc32;
        };


        template <typename InitFunc>
    void send_kernel_message(const std::string& message_id, InitFunc init_payload) {
            veyron::Envelope env;
            env.set_message_id(message_id);
            env.set_version(1);
            env.set_sender_id(this->plugin_id);

            init_payload(env);

            std::string protobuf_bytes;
            env.SerializeToString(&protobuf_bytes);

            std::vector<uint8_t> frame = pack_frame("kernel", protobuf_bytes);
            this->write_to_socket(frame.data(), frame.size());
        }

    };
}