"""Prepare leaf-event CPU tables from the locally installed WPA profile.

The resulting profile must still be accepted against an owned ETL on each WPT
version. No exporter output layout or measurement unit is inferred here.
"""

import copy
from pathlib import Path
import xml.etree.ElementTree as ET


NS = "http://tempuri.org/SerializableElement.xsd"
SAMPLED = "b855361e-7be0-4bc8-a754-3e8507715ca5"
PRECISE = "c58f5fea-0319-4046-932d-e695ebe20b47"
SAMPLE_KEYS = ["Process", "Thread ID", "CPU", "TimeStamp", "Address", "Module", "Function", "Stack"]
PRECISE_KEYS = ["New Process", "New Thread Id", "Cpu", "Switch-In Time",
                "Last Switch-Out Time", "Ready Time", "Next Switch-Out Time",
                "New Prev State", "New Prev Wait Reason", "New Thread Stack", "Ready Thread Stack"]


def local(element):
    return element.tag.rsplit("}", 1)[-1]


def prepare_profile(source, destination):
    """Keep vendor graph/metadata definitions, select unambiguous event columns."""
    source, destination = Path(source).resolve(), Path(destination).resolve()
    tree = ET.parse(source)
    root = tree.getroot()
    graphs = {node.get("Guid"): copy.deepcopy(node) for node in root.iter()
              if local(node) == "Graph" and node.get("Guid") in (SAMPLED, PRECISE)}
    if set(graphs) != {SAMPLED, PRECISE}:
        raise ValueError("WPA source profile must contain the sampled and precise CPU graphs")
    for guid, keys in ((SAMPLED, SAMPLE_KEYS), (PRECISE, PRECISE_KEYS)):
        graph = graphs[guid]
        preset = next((node for node in graph if local(node) == "Preset"), None)
        if preset is None:
            raise ValueError("WPA graph has no preset")
        columns = next(node for node in preset if local(node) == "Columns")
        selected = []
        for name in keys + (["Count", "Weight"] if guid == SAMPLED else ["Count"]):
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
                          "GraphFilterColumnGuid"):
            preset.attrib.pop(attribute, None)
        preset.set("Name", "Renderer sampled events" if guid == SAMPLED else "Renderer scheduler events")
        preset.set("InitialFilterShouldKeep", "true")
        preset.set("KeyColumnCount", str(len(keys)))
        preset.set("GraphColumnCount", str(len(selected)))
        preset.set("RightFrozenColumnCount", str(len(selected)))
        graph.set("LayoutStyle", "DataTable")
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
    container.extend(graphs[guid] for guid in (SAMPLED, PRECISE))
    destination.parent.mkdir(parents=True, exist_ok=True)
    ET.register_namespace("", NS)
    tree.write(destination, encoding="utf-8", xml_declaration=True)
    return destination
