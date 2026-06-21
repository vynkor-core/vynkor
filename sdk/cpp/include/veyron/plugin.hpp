#pragma once

#include <stdexcept>
#include <string>

#include "veyron/client.hpp"

namespace veyron {

class Plugin {
public:
    explicit Plugin(std::string plugin_id,
                    std::string socket_path = "/tmp/veyron.sock")
        : plugin_id_(std::move(plugin_id))
        , client_(std::move(socket_path)) {}

    virtual ~Plugin() = default;

    virtual void on_init() {}
    virtual void on_message(const Envelope& env) = 0;
    virtual void on_shutdown() {}

    void run() {
        client_.connect();

        Envelope ack = client_.register_plugin(plugin_id_);
        if (!ack.plugin_register_ack().accepted()) {
            throw std::runtime_error(
                "veyron: registration rejected: " +
                ack.plugin_register_ack().reject_reason());
        }

        on_init();
        try {
            while (true) {
                Envelope env = client_.recv();
                if (env.has_plugin_shutdown()) break;
                on_message(env);
            }
        } catch (...) {
            on_shutdown();
            client_.close();
            throw;
        }
        on_shutdown();
        client_.close();
    }

protected:
    std::string  plugin_id_;
    VeyronClient client_;
};

} // namespace veyron
