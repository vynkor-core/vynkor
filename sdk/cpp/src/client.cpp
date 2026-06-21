#include "veyron/client.hpp"

#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

#include <chrono>
#include <cstring>
#include <stdexcept>

namespace veyron {

VeyronClient::VeyronClient(std::string socket_path)
    : socket_path_(std::move(socket_path)) {}

VeyronClient::~VeyronClient() { close(); }

void VeyronClient::connect() {
    fd_ = ::socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd_ < 0)
        throw std::runtime_error("veyron: socket() failed");

    struct sockaddr_un addr{};
    addr.sun_family = AF_UNIX;
    std::strncpy(addr.sun_path, socket_path_.c_str(), sizeof(addr.sun_path) - 1);

    if (::connect(fd_, reinterpret_cast<struct sockaddr*>(&addr), sizeof(addr)) != 0) {
        ::close(fd_);
        fd_ = -1;
        throw std::runtime_error("veyron: connect() failed: " + socket_path_);
    }
}

void VeyronClient::close() {
    if (fd_ >= 0) {
        ::close(fd_);
        fd_ = -1;
    }
}

Envelope VeyronClient::register_plugin(const std::string& plugin_id,
                                        const std::string& jwt_token) {
    plugin_id_ = plugin_id;

    Envelope env;
    auto* reg = env.mutable_plugin_register();
    reg->set_plugin_id(plugin_id);
    reg->set_jwt_token(jwt_token);

    send_envelope("kernel", env);
    return recv();
}

void VeyronClient::send(const std::string& target, const Envelope& env) {
    send_envelope(target, env);
}

Envelope VeyronClient::recv() {
    auto payload = read_frame(fd_);
    Envelope env;
    if (!env.ParseFromArray(payload.data(), static_cast<int>(payload.size())))
        throw std::runtime_error("veyron: protobuf parse failed");
    return env;
}

void VeyronClient::subscribe(const std::vector<std::string>& event_types) {
    Envelope env;
    auto* sub = env.mutable_subscribe();
    for (const auto& t : event_types)
        sub->add_event_types(t);
    send_envelope("kernel", env);
}

double VeyronClient::ping() {
    using clk = std::chrono::steady_clock;
    using sys = std::chrono::system_clock;

    Envelope env;
    auto* p = env.mutable_ping();
    p->set_timestamp(static_cast<uint64_t>(
        std::chrono::duration_cast<std::chrono::milliseconds>(
            sys::now().time_since_epoch()).count()));

    auto t0 = clk::now();
    send_envelope("kernel", env);
    recv();  // discard Pong
    return std::chrono::duration<double>(clk::now() - t0).count();
}

void VeyronClient::send_envelope(const std::string& target, const Envelope& env) {
    std::string bytes;
    env.SerializeToString(&bytes);
    auto frame = pack_frame(target, bytes);

    const uint8_t* ptr = frame.data();
    size_t remaining   = frame.size();
    while (remaining > 0) {
        ssize_t written = ::write(fd_, ptr, remaining);
        if (written <= 0)
            throw std::runtime_error("veyron: write() failed");
        ptr       += written;
        remaining -= static_cast<size_t>(written);
    }
}

} // namespace veyron
