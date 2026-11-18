
// Appended to the pinned SDK runner in the calibration image. The sixth
// streaming state is [1, 32, 16, 1] in the runner's external NCHW buffer.
extern "C" bool goblin_read_step(const ww_runner_t *runner, int8_t step[32])
{
    constexpr size_t channels = 32;
    constexpr size_t length = 16;
    const int8_t *state = runner->state + WW_MODEL_STATE_BYTES - channels * length;
    // The previous block exports 22 steps in the adjacent state. Verify the
    // programmed delays directly against that history after every inference.
    constexpr size_t history = 22;
    const int8_t *previous = state - channels * history;
    bool matches = true;
    for (size_t c = 0; c < channels; ++c) {
        step[c] = state[c * length + length - 1];
        const size_t delay = (c % 4) * 3;
        matches &= step[c] == previous[c * history + history - 1 - delay];
    }
    return matches;
}
