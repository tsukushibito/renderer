/* Consumer-owned bridge; upstream mikktspace.c/h are unmodified. */
#include "mikktspace.h"
#include <limits.h>
#include <stddef.h>
#include <string.h>

#ifdef _WIN32
#define EXPORT __declspec(dllexport)
#else
#define EXPORT __attribute__((visibility("default")))
#endif

typedef struct {
    int faces;
    const float *vertices; /* unindexed pos3 normal3 uv2 */
    float *tangents;       /* one xyzw per corner */
} Input;

static int faces(const SMikkTSpaceContext *ctx) {
    return ((const Input *)ctx->m_pUserData)->faces;
}
static int corners(const SMikkTSpaceContext *ctx, int face) {
    (void)ctx;
    (void)face;
    return 3;
}
static const float *vertex(const SMikkTSpaceContext *ctx, int face, int corner) {
    const Input *input = (const Input *)ctx->m_pUserData;
    return input->vertices + ((size_t)face * 3 + corner) * 8;
}
static void position(const SMikkTSpaceContext *ctx, float out[], int face, int corner) {
    memcpy(out, vertex(ctx, face, corner), 3 * sizeof(float));
}
static void normal(const SMikkTSpaceContext *ctx, float out[], int face, int corner) {
    memcpy(out, vertex(ctx, face, corner) + 3, 3 * sizeof(float));
}
static void uv(const SMikkTSpaceContext *ctx, float out[], int face, int corner) {
    memcpy(out, vertex(ctx, face, corner) + 6, 2 * sizeof(float));
}
static void tangent(const SMikkTSpaceContext *ctx, const float value[], float sign,
                    int face, int corner) {
    Input *input = (Input *)ctx->m_pUserData;
    float *out = input->tangents + ((size_t)face * 3 + corner) * 4;
    memcpy(out, value, 3 * sizeof(float));
    out[3] = sign;
}

EXPORT int bistro_mikktspace(const float *vertices, int corner_count, float *output) {
    if (!vertices || !output || corner_count <= 0 || corner_count % 3 != 0 ||
        corner_count > INT_MAX / 8) return -1;
    Input input = {corner_count / 3, vertices, output};
    SMikkTSpaceInterface interface = {faces, corners, position, normal, uv, tangent, NULL};
    SMikkTSpaceContext context = {&interface, &input};
    return genTangSpaceDefault(&context) ? 1 : 0;
}
