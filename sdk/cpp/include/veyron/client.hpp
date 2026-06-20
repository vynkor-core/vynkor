#pragma once

#include <string>
#include <vector>
#include "veyron/framing.hpp"
#include "proto/veyron_protocol.pb.h"

namespace veyron {

class client {
    template <typename InitFunc>
    void send_kernel_message(const std::string& message_id, InitFunc init_payload) {
        veyron::Envelope env;
        env.set_message_id(message_id);
        env.set_version(1);
        env.set_sender_id(this->plugin_id);

        init_payload(env);

        std::string protobuf_bytes;
        env.SerializeToString(&protobuf_bytes);

        std::vector<uint8_t> frame = veyron::pack_frame("kernel", protobuf_bytes);
        this->write_to_socket(frame.data(), frame.size());
    }

    std::string plugin_id;
    virtual void write_to_socket(const uint8_t* data, size_t len) = 0;
};

} // namespace veyron
