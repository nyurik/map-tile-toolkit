#!/usr/bin/env python3
"""Refresh `tests/visualize.qgz` so its layers are exactly the GeoJSON files under `tests/`.

The layer tree mirrors the directory tree: one group per directory, sorted, holding its files
(sorted) above its subdirectories. Layers that already exist keep everything set on them in QGIS
(style, visibility, ...); new files get a layer cloned from one of the same geometry type with a
color derived from their path; layers whose file is gone are dropped. A file holding several
geometry types becomes one layer per type, as QGIS itself does. Needs only the standard library.

Usage: python3 tests/update_visualize.py [path/to/visualize.qgz]
"""

import colorsys
import copy
import hashlib
import json
import sys
import uuid
import xml.etree.ElementTree as ET
import zipfile
from pathlib import Path

TESTS = Path(__file__).resolve().parent
QGZ = Path(sys.argv[1]) if len(sys.argv) > 1 else TESTS / "visualize.qgz"

# GeoJSON geometry type -> (QGIS geometry kind, OGR sublayer name)
KINDS = {
    "Point": ("Point", "Point"),
    "MultiPoint": ("Point", "Point"),
    "LineString": ("Line", "LineString"),
    "MultiLineString": ("Line", "LineString"),
    "Polygon": ("Polygon", "Polygon"),
    "MultiPolygon": ("Polygon", "Polygon"),
}
# Field-specific parts of a cloned layer, emptied so QGIS rebuilds them from the new file.
FIELD_ELEMENTS = ["fieldConfiguration", "aliases", "defaults", "constraints", "constraintExpressions"]


def walk_coords(coords, out):
    if coords and isinstance(coords[0], (int, float)):
        out.append(coords[:2])
    else:
        for c in coords or []:
            walk_coords(c, out)


def file_layers(path):
    """One `(source, kind, wkb, extent)` per geometry kind in the file, points first."""
    data = json.loads(path.read_text())
    features = data.get("features", []) if data.get("type") == "FeatureCollection" else [data]
    by_kind = {}
    for f in features:
        geom = f.get("geometry") or {}
        if geom.get("type") not in KINDS:
            continue
        kind, sublayer = KINDS[geom["type"]]
        entry = by_kind.setdefault(kind, {"sublayer": sublayer, "types": set(), "coords": []})
        entry["types"].add(geom["type"])
        walk_coords(geom.get("coordinates"), entry["coords"])
    rel = "./" + path.relative_to(TESTS).as_posix()
    layers = []
    for kind in ("Point", "Line", "Polygon"):
        if kind not in by_kind:
            continue
        e = by_kind[kind]
        source = rel if len(by_kind) == 1 else f"{rel}|geometrytype={e['sublayer']}"
        multi = any(t.startswith("Multi") for t in e["types"])
        wkb = ("Multi" if multi else "") + e["sublayer"]
        xs = [c[0] for c in e["coords"]] or [0]
        ys = [c[1] for c in e["coords"]] or [0]
        layers.append((source, kind, wkb, (min(xs), min(ys), max(xs), max(ys))))
    return layers


def color_for(source):
    """A stable, moderately saturated color per source, like QGIS's random layer colors."""
    h = int(hashlib.md5(source.encode()).hexdigest()[:8], 16)
    r, g, b = colorsys.hsv_to_rgb((h % 360) / 360, 0.5 + (h >> 9) % 30 / 100, 0.75 + (h >> 17) % 20 / 100)
    rgb = [round(c * 255) for c in (r, g, b)]
    return ",".join(map(str, rgb)) + ",255,rgb:" + ",".join(f"{c / 255:.7g}" for c in rgb) + ",1"


def set_extent(layer, extent):
    for tag in ("extent", "wgs84extent"):
        e = layer.find(tag)
        if e is not None:
            for name, value in zip(("xmin", "ymin", "xmax", "ymax"), extent):
                e.find(name).text = f"{value:g}"


def new_layer(template, source, kind, wkb, extent):
    layer = copy.deepcopy(template)
    name = Path(source.split("|")[0]).name.removesuffix(".geojson")
    layer_id = name.replace("-", "_").replace(".", "_") + "_" + str(uuid.uuid4()).replace("-", "_")
    layer.find("id").text = layer_id
    layer.find("datasource").text = source
    layer.find("layername").text = name
    layer.set("geometry", kind)
    layer.set("wkbType", wkb)
    set_extent(layer, extent)
    for tag in FIELD_ELEMENTS:
        e = layer.find(tag)
        if e is not None:
            for child in list(e):
                e.remove(child)
    color = color_for(source)
    for option in layer.iter("Option"):
        if option.get("name") in ("color", "line_color") and option.get("type") == "QString":
            option.set("value", color)
    return layer, layer_id, name


def main():
    with zipfile.ZipFile(QGZ) as z:
        names = z.namelist()
        qgs_name = next(n for n in names if n.endswith(".qgs"))
        others = {n: z.read(n) for n in names if n != qgs_name}
        root = ET.fromstring(z.read(qgs_name))

    project_layers = root.find("projectlayers")
    old_layers = {m.find("datasource").text: m for m in project_layers}
    old_tree = root.find("layer-tree-group")
    old_nodes = {n.get("source"): n for n in old_tree.iter("layer-tree-layer")}
    old_groups = {}

    def index_groups(group, path):
        for g in group.findall("layer-tree-group"):
            p = path + (g.get("name"),)
            old_groups[p] = g
            index_groups(g, p)

    index_groups(old_tree, ())
    # A plain-styled layer of each kind to clone for new files (categorized ones like the grid are
    # styled for their own attributes).
    templates = {}
    for m in project_layers:
        renderer = m.find("renderer-v2")
        if renderer is not None and renderer.get("type") == "singleSymbol":
            templates.setdefault(m.get("geometry"), m)
    template_node = next(iter(old_nodes.values()))

    files = sorted(TESTS.rglob("*.geojson"), key=lambda p: p.relative_to(TESTS).parts)
    kept, added = {}, []
    tree = {"files": [], "dirs": {}}
    for path in files:
        node = tree
        for part in path.relative_to(TESTS).parts[:-1]:
            node = node["dirs"].setdefault(part, {"files": [], "dirs": {}})
        for source, kind, wkb, extent in file_layers(path):
            if source in old_layers:
                layer = old_layers[source]
                set_extent(layer, extent)
                kept[source] = layer
                node["files"].append((source, layer.find("id").text, old_nodes.get(source)))
            else:
                if kind not in templates:
                    sys.exit(f"no {kind} layer to clone for {source}; add one in QGIS first")
                layer, layer_id, name = new_layer(templates[kind], source, kind, wkb, extent)
                kept[source] = layer
                added.append(source)
                node["files"].append((source, layer_id, None))

    removed = sorted(set(old_layers) - set(kept))
    layer_ids = [kept[s].find("id").text for s in kept]

    # Layers, in tree order.
    for m in list(project_layers):
        project_layers.remove(m)
    order = []

    def build(node, path, parent):
        for source, layer_id, old in node["files"]:
            layer = kept[source]
            project_layers.append(layer)
            order.append(layer_id)
            if old is not None:
                parent.append(old)
            else:
                n = copy.deepcopy(template_node)
                n.attrib.update(
                    checked="Qt::Checked",
                    expanded="0",
                    id=layer_id,
                    name=layer.find("layername").text,
                    source=source,
                    providerKey="ogr",
                )
                parent.append(n)
        for name in sorted(node["dirs"]):
            p = path + (name,)
            old = old_groups.get(p)
            g = ET.SubElement(parent, "layer-tree-group")
            if old is not None:
                g.attrib.update(old.attrib)
            else:
                g.attrib.update(checked="Qt::Unchecked", expanded="0", groupLayer="", name=name)
                g.set("wms-group-request-mode", "Normal")
            props = old.find("customproperties") if old is not None else None
            g.append(copy.deepcopy(props) if props is not None else ET.fromstring("<customproperties><Option/></customproperties>"))
            build(node["dirs"][name], p, g)

    for child in list(old_tree):
        if child.tag != "customproperties":
            old_tree.remove(child)
    build(tree, (), old_tree)

    layerorder = root.find("layerorder")
    for child in list(layerorder):
        layerorder.remove(child)
    for layer_id in order:
        ET.SubElement(layerorder, "layer", id=layer_id)

    snapping = root.find("snapping-settings/individual-layer-settings")
    if snapping is not None:
        settings = {s.get("id"): s for s in snapping}
        template_setting = next(iter(settings.values()), None)
        for child in list(snapping):
            snapping.remove(child)
        for layer_id in order:
            s = settings.get(layer_id)
            if s is None and template_setting is not None:
                s = copy.deepcopy(template_setting)
                s.set("id", layer_id)
            if s is not None:
                snapping.append(s)

    # The legacy `<legend>` mirror of the tree is rewritten by QGIS on the next save.
    legend = root.find("legend")
    if legend is not None:
        root.remove(legend)

    xml = ET.tostring(root, encoding="unicode")
    with zipfile.ZipFile(QGZ, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr(qgs_name, "<!DOCTYPE qgis PUBLIC 'http://mrcc.com/qgis.dtd' 'SYSTEM'>\n" + xml)
        for name, data in others.items():
            z.writestr(name, data)

    print(f"{QGZ.name}: {len(layer_ids)} layers ({len(added)} added, {len(removed)} removed)")
    for s in added:
        print(f"  + {s}")
    for s in removed:
        print(f"  - {s}")


if __name__ == "__main__":
    main()
