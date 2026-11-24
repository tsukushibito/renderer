// Capture one presented frame and report native event timings from its RenderDoc replay.
// Build against the headers and DLL for the same RenderDoc release (see examples guide).
#include <cmath>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <map>
#include <string>
#include "renderdoc_replay.h"

REPLAY_PROGRAM_MARKER()

namespace {
using Labels = std::map<uint32_t, std::string>;

void index_actions(const rdcarray<ActionDescription>& actions, const std::string& parent, Labels& labels) {
    for (const auto& action : actions) {
        auto name = parent;
        if ((action.flags & ActionFlags::PushMarker) != ActionFlags::NoFlags && !action.customName.empty()) {
            if (!name.empty()) name += "/";
            name += action.customName.c_str();
        }
        labels.emplace(action.eventId, name);
        index_actions(action.children, name, labels);
    }
}

int capture(char** args) {
    const uint32_t frame = std::stoul(args[3]);
    auto result = RENDERDOC_ExecuteAndInject(args[4], args[5], args[6], {}, args[2], {}, false);
    if (result.result.code != ResultCode::Succeeded) {
        std::cerr << static_cast<int>(result.result.code) << std::endl;
        return 1;
    }
    auto* target = RENDERDOC_CreateTargetControl("", result.ident, "survivor-benchmark", false);
    if (!target) return 1;
    target->QueueCapture(frame, 1);
    bool captured = false;
    while (target->Connected()) {
        auto message = target->ReceiveMessage();
        if (message.type == TargetControlMessageType::NewCapture) {
            captured = true;
            std::cout << message.newCapture.path.c_str() << std::endl;
        }
    }
    target->Shutdown();
    return captured ? 0 : 1;
}

int write_passes(IReplayController& replay, const char* output) {
    Labels labels;
    index_actions(replay.GetRootActions(), "", labels);
    const auto values = replay.FetchCounters({GPUCounter::EventGPUDuration});
    std::ofstream out(output);
    out << "event,marker_path,gpu_ms\n" << std::setprecision(12);
    for (const auto& value : values) {
        const auto label = labels.find(value.eventId);
        if (label == labels.end() || !std::isfinite(value.value.d) || value.value.d < 0.0)
            return 1;
        // std::quoted doubles embedded quotes when quote and escape are both '"'.
        out << value.eventId << ',' << std::quoted(label->second, '"', '"') << ',' << value.value.d * 1000.0 << '\n';
    }
    out.flush();
    return out.good() && !values.empty() ? 0 : 1;
}

int passes(char** args) {
    auto* file = RENDERDOC_OpenCaptureFile();
    auto open = file->OpenFile(args[2], "", nullptr);
    int status = 1;
    if (open.code == ResultCode::Succeeded) {
        auto replay = file->OpenCapture({}, nullptr);
        if (replay.first.code == ResultCode::Succeeded) {
            status = write_passes(*replay.second, args[3]);
            replay.second->Shutdown();
        } else {
            std::cerr << static_cast<int>(replay.first.code) << std::endl;
        }
    } else {
        std::cerr << static_cast<int>(open.code) << std::endl;
    }
    file->Shutdown();
    return status;
}
}

int main(int argc, char** argv) {
    RENDERDOC_InitialiseReplay({}, {});
    int status = 1;
    if (argc == 7 && std::string(argv[1]) == "capture") status = capture(argv);
    else if (argc == 4 && std::string(argv[1]) == "passes") status = passes(argv);
    else std::cerr << "capture <template> <frame> <exe> <cwd> <game arguments> | passes <capture.rdc> <events.csv>\n";
    RENDERDOC_ShutdownReplay();
    return status;
}

