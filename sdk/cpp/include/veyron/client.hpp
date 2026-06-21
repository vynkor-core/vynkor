#pragma once

#include <cstdint>
#include <string>
#include <vector>

#include "veyron/framing.hpp"
#include "veyron_protocol.pb.h"

namespace veyron {

class VeyronClient {
public:
    explicit VeyronClient(std::string socket_path);
    ~VeyronClient();

    void connect();
    void close();

    // Send PluginRegister, return the RegisterAck envelope.
    Envelope register_plugin(const std::string& plugin_id,
                             const std::string& jwt_token = "");

    void    send(const std::string& target, const Envelope& env);
    Envelope recv();

    void   subscribe(const std::vector<std::string>& event_types);
    double ping();  // returns round-trip time in seconds

private:
    std::string socket_path_;
    int         fd_ = -1;
    std::string plugin_id_;

    void send_envelope(const std::string& target, const Envelope& env);
};

} // namespace veyron
