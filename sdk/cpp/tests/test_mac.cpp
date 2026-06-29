#include <gtest/gtest.h>
#include "veyron/mac.hpp"

using namespace veyron;

TEST(DeriveSessionKey, Deterministic) {
    std::vector<uint8_t> secret = {'s','e','c','r','e','t'};
    std::vector<uint8_t> nonce  = {'n','o','n','c','e','-','0','1','2','3','4','5','6','7','8','9'};
    auto k1 = derive_session_key(secret, nonce, "plugin-a");
    auto k2 = derive_session_key(secret, nonce, "plugin-a");
    EXPECT_EQ(k1, k2);
}

TEST(DeriveSessionKey, InputSensitive) {
    std::vector<uint8_t> secret = {'s','e','c','r','e','t'};
    std::vector<uint8_t> nonce  = {'n','o','n','c','e','-','0','1','2','3','4','5','6','7','8','9'};
    auto base = derive_session_key(secret, nonce, "plugin-a");

    std::vector<uint8_t> other_secret = {'o','t','h','e','r','!'};
    EXPECT_NE(base, derive_session_key(other_secret, nonce, "plugin-a"));

    std::vector<uint8_t> other_nonce = {'x','x','x','x','x','x','x','x','x','x','x','x','x','x','x','x'};
    EXPECT_NE(base, derive_session_key(secret, other_nonce, "plugin-a"));

    EXPECT_NE(base, derive_session_key(secret, nonce, "plugin-b"));
}

TEST(DeriveSessionKey, Matches32Bytes) {
    std::vector<uint8_t> secret = {'s'};
    std::vector<uint8_t> nonce(16, 0xAB);
    auto key = derive_session_key(secret, nonce, "p");
    EXPECT_EQ(key.size(), size_t(32));
}

TEST(ComputeVerifyTag, RoundTrip) {
    std::vector<uint8_t> secret = {'s','e','c','r','e','t'};
    std::vector<uint8_t> nonce(16, 0x01);
    auto key = derive_session_key(secret, nonce, "p");

    uint8_t header[44] = {};
    for (int i = 0; i < 44; ++i) header[i] = static_cast<uint8_t>(i);
    const uint8_t payload[] = {'h','e','l','l','o'};

    auto tag = compute_tag(key, header, 44, payload, 5);
    EXPECT_TRUE(verify_tag(key, header, 44, payload, 5, tag.data(), 32));
}

TEST(ComputeVerifyTag, TamperedPayloadRejected) {
    std::vector<uint8_t> secret = {'s','e','c','r','e','t'};
    std::vector<uint8_t> nonce(16, 0x01);
    auto key = derive_session_key(secret, nonce, "p");

    uint8_t header[44] = {};
    const uint8_t payload[]  = {'h','e','l','l','o'};
    const uint8_t bad_pl[]   = {'h','e','l','l','x'};

    auto tag = compute_tag(key, header, 44, payload, 5);
    EXPECT_FALSE(verify_tag(key, header, 44, bad_pl, 5, tag.data(), 32));
}

TEST(ComputeVerifyTag, TamperedHeaderRejected) {
    std::vector<uint8_t> secret = {'s','e','c','r','e','t'};
    std::vector<uint8_t> nonce(16, 0x01);
    auto key = derive_session_key(secret, nonce, "p");

    uint8_t header[44] = {};
    uint8_t bad_hdr[44] = {};
    bad_hdr[0] = 0xFF;
    const uint8_t payload[] = {'h','e','l','l','o'};

    auto tag = compute_tag(key, header, 44, payload, 5);
    EXPECT_FALSE(verify_tag(key, bad_hdr, 44, payload, 5, tag.data(), 32));
}

TEST(ComputeVerifyTag, WrongKeyRejected) {
    std::vector<uint8_t> secret = {'s','e','c','r','e','t'};
    std::vector<uint8_t> nonce_a(16, 0x01);
    std::vector<uint8_t> nonce_b(16, 0x02);
    auto key_a = derive_session_key(secret, nonce_a, "p");
    auto key_b = derive_session_key(secret, nonce_b, "p");

    uint8_t header[44] = {};
    const uint8_t payload[] = {'h','e','l','l','o'};
    auto tag = compute_tag(key_a, header, 44, payload, 5);
    EXPECT_FALSE(verify_tag(key_b, header, 44, payload, 5, tag.data(), 32));
}

// ---------------------------------------------------------------------------
// Framing MAC tests (Task 4)
// ---------------------------------------------------------------------------
#include "veyron/framing.hpp"
#include <arpa/inet.h>
#include <cstring>
#include <unistd.h>

static std::pair<int,int> make_pipe() {
    int fds[2];
    if (::pipe(fds) != 0) throw std::runtime_error("pipe failed");
    return {fds[0], fds[1]};
}

TEST(FramingMac, PackFrameMacSetsFlag) {
    std::vector<uint8_t> secret = {'s','e','c','r','e','t'};
    std::vector<uint8_t> nonce(16, 0x01);
    auto key = veyron::derive_session_key(secret, nonce, "tgt");

    std::vector<uint8_t> payload = {'h','e','l','l','o'};
    auto frame = veyron::pack_frame_mac("tgt", payload, key);

    ASSERT_EQ(frame.size(), size_t(44 + 5 + 32));

    uint16_t flags;
    std::memcpy(&flags, frame.data() + 2, 2);
    flags = ntohs(flags);
    EXPECT_TRUE(flags & veyron::FLAG_MAC_PRESENT);
}

TEST(FramingMac, ReadFrameFullVerifiesValidMac) {
    std::vector<uint8_t> secret = {'s','e','c','r','e','t'};
    std::vector<uint8_t> nonce(16, 0x01);
    auto key = veyron::derive_session_key(secret, nonce, "tgt");

    std::vector<uint8_t> payload = {'h','e','l','l','o'};
    auto frame = veyron::pack_frame_mac("tgt", payload, key);

    auto [read_fd, write_fd] = make_pipe();
    ::write(write_fd, frame.data(), frame.size());
    ::close(write_fd);

    auto result = veyron::read_frame_full(read_fd, &key);
    ::close(read_fd);

    EXPECT_EQ(result.payload, payload);
    EXPECT_TRUE(result.has_mac);
}

TEST(FramingMac, ReadFrameFullRejectsTamperedTag) {
    std::vector<uint8_t> secret = {'s','e','c','r','e','t'};
    std::vector<uint8_t> nonce(16, 0x01);
    auto key = veyron::derive_session_key(secret, nonce, "tgt");

    std::vector<uint8_t> payload = {'h','i'};
    auto frame = veyron::pack_frame_mac("tgt", payload, key);
    frame.back() ^= 0xFF;

    auto [read_fd, write_fd] = make_pipe();
    ::write(write_fd, frame.data(), frame.size());
    ::close(write_fd);

    EXPECT_THROW(veyron::read_frame_full(read_fd, &key), std::runtime_error);
    ::close(read_fd);
}

TEST(FramingMac, ReadFrameFullNoKeySkipsVerification) {
    std::vector<uint8_t> secret = {'s','e','c','r','e','t'};
    std::vector<uint8_t> nonce(16, 0x01);
    auto key = veyron::derive_session_key(secret, nonce, "tgt");

    std::vector<uint8_t> payload = {'n','o','k','e','y'};
    auto frame = veyron::pack_frame_mac("tgt", payload, key);

    auto [read_fd, write_fd] = make_pipe();
    ::write(write_fd, frame.data(), frame.size());
    ::close(write_fd);

    auto result = veyron::read_frame_full(read_fd, nullptr);
    ::close(read_fd);

    EXPECT_EQ(result.payload, payload);
    EXPECT_TRUE(result.has_mac);
}

// ---------------------------------------------------------------------------
// Client MAC test (Task 5)
// ---------------------------------------------------------------------------
#include "veyron/client.hpp"

TEST(VeyronClientMac, DeriveSessionKeyAfterMockAck) {
    std::vector<uint8_t> secret = {'j','w','t','s','e','c'};
    std::vector<uint8_t> nonce(16, 0xBE);

    Envelope ack_env;
    auto* ack = ack_env.mutable_plugin_register_ack();
    ack->set_accepted(true);
    ack->set_session_nonce(std::string(nonce.begin(), nonce.end()));
    std::string serialized;
    ack_env.SerializeToString(&serialized);

    auto frame = veyron::pack_frame("plugin-test", serialized);

    auto [read_fd, write_fd] = make_pipe();
    ::write(write_fd, frame.data(), frame.size());
    ::close(write_fd);

    auto expected_key = veyron::derive_session_key(secret, nonce, "plugin-test");

    auto result = veyron::read_frame_full(read_fd, nullptr);
    ::close(read_fd);

    Envelope parsed;
    ASSERT_TRUE(parsed.ParseFromArray(result.payload.data(),
                                     static_cast<int>(result.payload.size())));
    ASSERT_TRUE(parsed.has_plugin_register_ack());
    const auto& parsed_ack = parsed.plugin_register_ack();
    ASSERT_TRUE(parsed_ack.accepted());

    auto raw_nonce = parsed_ack.session_nonce();
    std::vector<uint8_t> parsed_nonce(raw_nonce.begin(), raw_nonce.end());
    auto derived = veyron::derive_session_key(secret, parsed_nonce, "plugin-test");
    EXPECT_EQ(derived, expected_key);
}

int main(int argc, char** argv) {
    ::testing::InitGoogleTest(&argc, argv);
    return RUN_ALL_TESTS();
}
