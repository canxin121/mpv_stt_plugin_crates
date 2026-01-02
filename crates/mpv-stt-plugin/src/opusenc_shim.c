#include <stdint.h>

typedef int32_t opus_int32;
typedef uint32_t opus_uint32;

typedef void (*ope_packet_func)(void *user_data,
                                const unsigned char *packet_ptr,
                                opus_int32 packet_len,
                                opus_uint32 flags);

typedef struct OggOpusEnc OggOpusEnc;

int ope_encoder_ctl(OggOpusEnc *enc, int request, ...);

int mpv_stt_ope_set_packet_callback(OggOpusEnc *enc,
                                    ope_packet_func cb,
                                    void *user_data) {
    return ope_encoder_ctl(enc, 14008, cb, user_data);
}
