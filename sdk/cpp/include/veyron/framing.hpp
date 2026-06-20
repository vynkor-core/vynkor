#pragma once

#include <cstdint>
#include <string>
#include <vector>

namespace veyron {

static constexpr uint16_t FRAME_MAGIC      = 0x5652;
static constexpr size_t   FRAME_HEADER_SIZE = 44;
static constexpr size_t   MAX_PAYLOAD_SIZE  = 1048576; // 1 MiB

// Wire format (all multi-byte fields big-endian):
//   [0..1]   magic   uint16  = 0x5652
//   [2..3]   flags   uint16  = 0 (reserved)
//   [4..7]   length  uint32  = payload byte count
//   [8..39]  target  char[32] null-padded plugin_id / "kernel" / "*"
//   [40..43] crc32   uint32  = CRC-32/ISO-HDLC of payload
//   [44..]   payload

// CRC-32/ISO-HDLC — same polynomial as zlib crc32 / crc32fast
uint32_t veyron_crc32(const uint8_t* data, size_t len);

// Build a complete wire frame (header + payload).
// target: destination id, max 32 bytes (truncated if longer)
// payload: serialised protobuf bytes
std::vector<uint8_t> pack_frame(const std::string& target,
                                const std::vector<uint8_t>& payload);

// Overload accepting std::string payload (SerializeToString output)
std::vector<uint8_t> pack_frame(const std::string& target,
                                const std::string& payload);

// Read one frame from a blocking socket fd.
// Returns the payload bytes; throws std::runtime_error on any error.
std::vector<uint8_t> read_frame(int fd);

} // namespace veyron
