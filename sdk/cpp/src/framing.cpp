#include "veyron/framing.hpp"

#include <arpa/inet.h>  // htons, htonl, ntohs, ntohl
#include <sys/socket.h>
#include <unistd.h>

#include <array>
#include <cstring>
#include <stdexcept>
#include <string>
#include <vector>

namespace veyron {

// ---------------------------------------------------------------------------
// CRC-32/ISO-HDLC (IEEE 802.3) — identical to zlib crc32 / Rust crc32fast
// ---------------------------------------------------------------------------
static std::array<uint32_t, 256> build_crc32_table() {
    std::array<uint32_t, 256> t{};
    for (uint32_t i = 0; i < 256; ++i) {
        uint32_t c = i;
        for (int j = 0; j < 8; ++j)
            c = (c & 1u) ? (0xEDB88320u ^ (c >> 1)) : (c >> 1);
        t[i] = c;
    }
    return t;
}

uint32_t veyron_crc32(const uint8_t* data, size_t len) {
    static const auto table = build_crc32_table();
    uint32_t crc = 0xFFFFFFFFu;
    for (size_t i = 0; i < len; ++i)
        crc = table[(crc ^ data[i]) & 0xFFu] ^ (crc >> 8);
    return crc ^ 0xFFFFFFFFu;
}

// ---------------------------------------------------------------------------
// pack_frame
// ---------------------------------------------------------------------------
std::vector<uint8_t> pack_frame(const std::string& target,
                                const std::vector<uint8_t>& payload) {
    if (payload.size() > MAX_PAYLOAD_SIZE)
        throw std::runtime_error("veyron: payload exceeds 1 MiB limit");

    uint32_t crc = veyron_crc32(payload.data(), payload.size());

    uint8_t header[FRAME_HEADER_SIZE] = {};

    // [0..1] magic — big-endian
    const uint16_t magic_be = htons(FRAME_MAGIC);
    std::memcpy(header + 0, &magic_be, 2);

    // [2..3] flags — big-endian, reserved = 0
    const uint16_t flags_be = htons(0u);
    std::memcpy(header + 2, &flags_be, 2);

    // [4..7] payload length — big-endian
    const uint32_t len_be = htonl(static_cast<uint32_t>(payload.size()));
    std::memcpy(header + 4, &len_be, 4);

    // [8..39] target — null-padded to 32 bytes (already zeroed)
    const size_t copy_len = std::min(target.size(), size_t{32});
    std::memcpy(header + 8, target.data(), copy_len);

    // [40..43] CRC32 — big-endian
    const uint32_t crc_be = htonl(crc);
    std::memcpy(header + 40, &crc_be, 4);

    std::vector<uint8_t> frame;
    frame.reserve(FRAME_HEADER_SIZE + payload.size());
    frame.insert(frame.end(), header, header + FRAME_HEADER_SIZE);
    frame.insert(frame.end(), payload.begin(), payload.end());
    return frame;
}

std::vector<uint8_t> pack_frame(const std::string& target,
                                const std::string& payload) {
    return pack_frame(target,
                      std::vector<uint8_t>(payload.begin(), payload.end()));
}

// ---------------------------------------------------------------------------
// read_frame
// ---------------------------------------------------------------------------
static void recv_exact(int fd, uint8_t* buf, size_t n) {
    size_t total = 0;
    while (total < n) {
        const ssize_t r = ::recv(fd, buf + total, n - total, MSG_WAITALL);
        if (r <= 0)
            throw std::runtime_error("veyron: connection closed or recv error");
        total += static_cast<size_t>(r);
    }
}

std::vector<uint8_t> read_frame(int fd) {
    uint8_t header[FRAME_HEADER_SIZE];
    recv_exact(fd, header, FRAME_HEADER_SIZE);

    // Validate magic
    uint16_t magic;
    std::memcpy(&magic, header + 0, 2);
    if (ntohs(magic) != FRAME_MAGIC)
        throw std::runtime_error("veyron: invalid frame magic");

    // Payload length
    uint32_t length;
    std::memcpy(&length, header + 4, 4);
    length = ntohl(length);
    if (length > MAX_PAYLOAD_SIZE)
        throw std::runtime_error("veyron: frame payload exceeds 1 MiB limit");

    // Expected CRC32
    uint32_t expected_crc;
    std::memcpy(&expected_crc, header + 40, 4);
    expected_crc = ntohl(expected_crc);

    // Read payload
    std::vector<uint8_t> payload(length);
    if (length > 0)
        recv_exact(fd, payload.data(), length);

    // Validate CRC32
    const uint32_t actual_crc = veyron_crc32(payload.data(), payload.size());
    if (actual_crc != expected_crc)
        throw std::runtime_error("veyron: CRC32 mismatch");

    return payload;
}

} // namespace veyron
