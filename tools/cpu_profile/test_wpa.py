"""Portable WPA profile contracts; actual leaf/table acceptance uses owned ETL."""

from pathlib import Path
import tempfile
import unittest
import xml.etree.ElementTree as ET

from wpa import (IMAGE_COLUMNS, IMAGES, PRECISE, PRECISE_KEYS, PROCESS_COLUMNS,
                 PROCESSES, SAMPLED, SAMPLE_KEYS, local, prepare_profile)


class WpaContracts(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.cpu = self.vendor("cpu.wpaProfile", ((SAMPLED, SAMPLE_KEYS + ["Count", "Weight"]),
                                                (PRECISE, PRECISE_KEYS + ["Count"])))

    def vendor(self, name, graphs):
        root = ET.Element("WpaProfileContainer")
        container = ET.SubElement(ET.SubElement(root, "View"), "Graphs")
        for guid, names in graphs:
            graph = ET.SubElement(container, "Graph", Guid=guid)
            preset = ET.SubElement(graph, "Preset", Name="vendor", KeyColumnCount="9",
                                   LeftFrozenColumnCount="7", InitialFilterQuery="vendor filter",
                                   InitialExpansionQuery="vendor expansion",
                                   GraphFilterColumnName="stale grouping")
            ET.SubElement(ET.SubElement(preset, "MetadataEntries"), "MetadataEntry", Name="preserved")
            columns = ET.SubElement(preset, "Columns")
            for index, column_name in enumerate(names):
                ET.SubElement(columns, "Column", Name=column_name, Guid=f"{guid}-{index}",
                              SortPriority="1", IsVisible="false")
        path = self.directory / name
        ET.ElementTree(root).write(path, encoding="utf-8")
        return path

    def generate(self, **kwargs):
        output = prepare_profile(self.cpu, self.directory / "output.wpaProfile", **kwargs)
        return {graph.get("Guid"): graph for graph in ET.parse(output).getroot().iter()
                if local(graph) == "Graph"}

    def test_zero_keys_export_events_instead_of_collapsed_group_totals(self):
        graphs = self.generate()
        for guid, names in ((SAMPLED, SAMPLE_KEYS + ["Count", "Weight"]),
                            (PRECISE, PRECISE_KEYS + ["Count"])):
            preset = graphs[guid].find("Preset")
            self.assertEqual(preset.get("KeyColumnCount"), "0")
            self.assertEqual(preset.get("LeftFrozenColumnCount"), "0")
            self.assertNotIn("InitialExpansionQuery", preset.attrib)
            self.assertNotIn("InitialFilterQuery", preset.attrib)
            self.assertNotIn("GraphFilterColumnName", preset.attrib)
            self.assertEqual([node.get("Name") for node in preset.find("Columns")], names)
            self.assertIsNotNone(preset.find("MetadataEntries/MetadataEntry"))

    def test_exact_process_filter_uses_the_table_specific_identity(self):
        graphs = self.generate(process="profile_workload.exe (54092)")
        self.assertEqual(graphs[SAMPLED].find("Preset").get("InitialFilterQuery"),
                         '[Process]:="profile_workload.exe (54092)"')
        self.assertEqual(graphs[PRECISE].find("Preset").get("InitialFilterQuery"),
                         '[New Process]:="profile_workload.exe (54092)"')
        for process in ("profile_workload.exe", 'other.exe (1)" OR 1', "other.exe\n (1)"):
            with self.subTest(process=process), self.assertRaises(ValueError):
                self.generate(process=process)

    def test_identity_tables_preserve_actual_and_preferred_bases_separately(self):
        identity = self.vendor("identity.wpaProfile", ((IMAGES, IMAGE_COLUMNS),
                                                     (PROCESSES, PROCESS_COLUMNS)))
        graphs = self.generate(identity_source=identity, process="windowed.exe (54092)")
        self.assertEqual(set(graphs), {SAMPLED, PRECISE, IMAGES, PROCESSES})
        for guid, names in ((IMAGES, IMAGE_COLUMNS), (PROCESSES, PROCESS_COLUMNS)):
            preset = graphs[guid].find("Preset")
            self.assertEqual(preset.get("KeyColumnCount"), "0")
            self.assertEqual(preset.get("InitialFilterQuery"), '[Process]:="windowed.exe (54092)"')
            self.assertEqual([node.get("Name") for node in preset.find("Columns")], names)
        columns = list(graphs[IMAGES].find("Preset/Columns"))
        actual = next(node for node in columns if node.get("Name") == "Start Address")
        preferred = next(node for node in columns if node.get("Name") == "Default Start Address")
        self.assertNotEqual(actual.get("Guid"), preferred.get("Guid"))


if __name__ == "__main__":
    unittest.main()
