"""Prepare leaf-event CPU tables from the locally installed WPA profile.

The resulting profile must still be accepted against an owned ETL on each WPT
version. No exporter output layout or measurement unit is inferred here.
"""

import copy
from pathlib import Path
import re
import xml.etree.ElementTree as ET


NS = "http://tempuri.org/SerializableElement.xsd"
SAMPLED = "b855361e-7be0-4bc8-a754-3e8507715ca5"
PRECISE = "c58f5fea-0319-4046-932d-e695ebe20b47"
IMAGES = "a1df89f9-5b2a-449e-aaab-1bcc3fd25c84"
PROCESSES = "b186f02d-8d06-4bbb-9ad1-abf7985fb61a"
SAMPLE_KEYS = ["Process", "Thread ID", "CPU", "TimeStamp", "Address", "Module", "Function", "Stack"]
PRECISE_KEYS = ["New Process", "New Thread Id", "Old Process", "Old Thread Id", "Cpu", "Switch-In Time",
                "Last Switch-Out Time", "Ready Time", "Next Switch-Out Time",
                "New Prev State", "New Prev Wait Reason", "New Thread Stack", "Ready Thread Stack"]
IMAGE_COLUMNS = ["Process", "Process ID", "Image Name", "Image Path", "Start Address",
                 "End Address", "ImageSize", "Default Start Address", "Start Time", "End Time",
                 "Lifetime", "TimeDateStamp"]
PROCESS_COLUMNS = ["Process", "Process ID", "Process Name", "Parent Process ID", "Start Time",
                   "End Time", "Command Line", "Lifetime"]


def local(element):
    return element.tag.rsplit("}", 1)[-1]


def _graphs(root, guids):
    graphs = {node.get("Guid"): copy.deepcopy(node) for node in root.iter()
              if local(node) == "Graph" and node.get("Guid") in guids}
    if set(graphs) != set(guids):
        raise ValueError("WPA source profile is missing a required graph")
    return graphs


def _flatten(graph, names, title, process):
    preset = next((node for node in graph if local(node) == "Preset"), None)
    if preset is None:
        raise ValueError("WPA graph has no preset")
    columns = next(node for node in preset if local(node) == "Columns")
    selected = []
    for name in names:
        candidates = [node for node in columns if node.get("Name") == name]
        if name == "Stack":
            candidates = [node for node in candidates if not any(
                child.get("Mode") in ("StackTag", "FrameTags") for child in node)]
        if len(candidates) != 1:
            raise ValueError(f"WPA column {name!r} is missing or ambiguous")
        column = copy.deepcopy(candidates[0])
        column.set("IsVisible", "true")
        column.attrib.pop("SortPriority", None)
        selected.append(column)
    columns[:] = selected
    for attribute in ("InitialFilterQuery", "InitialSelectionQuery", "InitialExpansionQuery",
                      "GraphFilterColumnGuid", "GraphFilterColumnName"):
        preset.attrib.pop(attribute, None)
    preset.set("Name", title)
    preset.set("InitialFilterShouldKeep", "true")
    if process is not None:
        field = "New Process" if graph.get("Guid") == PRECISE else "Process"
        preset.set("InitialFilterQuery", f'[{field}]:="{process}"')
    # A key count equal to the selected columns still exports collapsed Process
    # subtotals. Zero keys requests the actual event rows, with complete stacks
    # in cells instead of expandable stack/group nodes. Confirmed on an owned
    # ETL with WPA 11.7.383.39833; other versions still require real-ETL acceptance.
    preset.set("KeyColumnCount", "0")
    preset.set("LeftFrozenColumnCount", "0")
    preset.set("GraphColumnCount", str(len(selected)))
    preset.set("RightFrozenColumnCount", str(len(selected)))
    graph.set("LayoutStyle", "DataTable")


def prepare_profile(source, destination, *, process=None, identity_source=None):
    """Select flat events, optionally one exact WPA ``name (PID)`` identity.

    ``identity_source`` may point to the installed WpaRuleMatchExporter profile
    to include Images and Processes tables. Images' Start Address is the actual
    load address. Retain the vendor Default Start Address separately; neither
    that field nor the build manifest's preferred PE base replaces Start Address.
    Time units come from the exported headers, never this profile's column names.
    """
    if process is not None and (not isinstance(process, str) or not re.fullmatch(
            r'[^\x00-\x1f"]+ \([0-9]+\)', process)):
        raise ValueError("process must be an exact WPA name (PID) without quotes or controls")
    source, destination = Path(source).resolve(), Path(destination).resolve()
    tree = ET.parse(source)
    root = tree.getroot()
    graphs = _graphs(root, (SAMPLED, PRECISE))
    for guid, keys in ((SAMPLED, SAMPLE_KEYS), (PRECISE, PRECISE_KEYS)):
        _flatten(graphs[guid], keys + (["Count", "Weight"] if guid == SAMPLED else ["Count"]),
                 "Renderer sampled events" if guid == SAMPLED else "Renderer scheduler events", process)
    if identity_source is not None:
        identity = ET.parse(Path(identity_source).resolve()).getroot()
        graphs.update(_graphs(identity, (IMAGES, PROCESSES)))
        _flatten(graphs[IMAGES], IMAGE_COLUMNS, "Renderer image lifetimes", process)
        _flatten(graphs[PROCESSES], PROCESS_COLUMNS, "Renderer process lifetimes", process)
    # Remove other graphs while retaining the vendor Sessions, metadata and
    # ancillary definitions. Make vendor FileReferences absolute before moving.
    for node in root.iter():
        if local(node) == "FileReference" and node.get("FileName"):
            filename = Path(node.get("FileName"))
            if not filename.is_absolute():
                node.set("FileName", str((source.parent / filename).resolve()))
    for node in root.iter():
        if local(node) == "Graphs":
            node[:] = []
    container = next((node for node in root.iter() if local(node) == "Graphs"), None)
    if container is None:
        raise ValueError("WPA source profile has no graph container")
    container.extend(graphs.values())
    destination.parent.mkdir(parents=True, exist_ok=True)
    ET.register_namespace("", NS)
    tree.write(destination, encoding="utf-8", xml_declaration=True)
    return destination
