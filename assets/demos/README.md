# Composition and conversation demo

`composition-chat.gif` is a 25-second screen capture of the actual browser client
connected to a real local Regulus core. It uses the Greeter profile from
`crates/modules/hello/examples/greeter.toml`, with no external model or mocked RPC.

The actions are: open Composition, open the Greeter workflow, inspect its Hello
node, return to Chat, run it, open Operate, select the execution, choose its `ask`
and `reply` conversation ports, send `Regulus contributors`, and receive
`Hello, Regulus contributors!`. The Hello module computes this response; it is not
an LLM response. No UI text or execution state was edited for the recording.

Captured from the development client after the conversation panel landed. The
recording therefore does not claim that the previously published release contains
this panel. The temporary core used its own home directory and example profile.

The browser was driven with Playwright at 1280×800. Screenshots were sampled during
real actions, with their elapsed timestamps preserved. FFmpeg converted them to a
1100-pixel-wide, five-frame-per-second GIF with a 128-color palette. There are no
synthetic frames, overlays, provider credentials or private project contents.

To refresh the recording, start an isolated core with the example profile, point a
development frontend at it using VITE_RPC_URL, perform those actions, capture the
browser, and check the final response visually before replacing the GIF. Keep the
recording between 20 and 30 seconds and below the repository's asset-size limit.
