#pragma once

#include <array>
#include <cstdint>
#include <string>
#include <vector>

#include "veyron/mac.hpp"

namespace veyron {

static constexpr uint16_t FRAME_MAGIC       = 0x5652;
static constexpr size_t   FRAME_HEADER_SIZE = 44;
static constexpr size_t   MAX_PAYLOAD_SIZE  = 1048576; // 1 MiB

// Wire format (all multi-byte fields big-endian):
//   [0..1]   magic   uint16  = 0x5652
//   [2..3]   flags   uint16  (FLAG_MAC_PRESENT = 0x0001)
//   [4..7]   length  uint32  = payload byte count
//   [8..39]  target  char[32] null-padded plugin_id / "kernel" / "*"
//   [40..43] crc32   uint32  = CRC-32/ISO-HDLC of payload
//   [44..]   payload
//   [44+N..] MAC tag (32 bytes) — present only when FLAG_MAC_PRESENT is set

// CRC-32/ISO-HDLC
uint32_t veyron_crc32(const uint8_t* data, size_t len);

// Build CRC-only frame (no MAC). Backward-compatible.
std::vector<uint8_t> pack_frame(const std::string& target,
                                const std::vector<uint8_t>& payload);
std::vector<uint8_t> pack_frame(const std::string& target,
                                const std::string& payload);

// Build a MAC frame: sets FLAG_MAC_PRESENT and appends 32-byte HMAC-SHA256 tag.
std::vector<uint8_t> pack_frame_mac(const std::string& target,
                                    const std::vector<uint8_t>& payload,
                                    const std::array<uint8_t, 32>& session_key);

// Result of read_frame_full.
struct FrameResult {
    std::vector<uint8_t>                    payload;
    uint16_t                                flags = 0;
    bool                                    has_mac = false;
    std::array<uint8_t, 32>                 mac = {};
    std::array<uint8_t, FRAME_HEADER_SIZE>  raw_header = {};
};

// Read one frame and return full FrameResult.
// If session_key is non-null and FLAG_MAC_PRESENT is set, verifies the MAC tag;
// throws std::runtime_error("veyron: MAC verification failed") on mismatch.
// If session_key is null, MAC bytes are read and stored but not verified.
FrameResult read_frame_full(int fd,
                            const std::array<uint8_t, 32>* session_key = nullptr);

// Backward-compat: returns only payload bytes. Does NOT verify MAC.
std::vector<uint8_t> read_frame(int fd);

} // namespace veyron
