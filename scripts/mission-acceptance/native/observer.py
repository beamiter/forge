"""Bounded, read-only public AT-SPI and screenshot OCR observation.

No Action, EditableText, Selection, Value, focus or mutation interface is used.
Accessible text outside its painted/clipped viewport is retained for diagnosis
but never enters visible_text or supplies an input target.
"""
import csv
import io
import re
import time

ACCESSIBLE = 'org.a11y.atspi.Accessible'
COMPONENT = 'org.a11y.atspi.Component'
TEXT = 'org.a11y.atspi.Text'
ROOT = '/org/a11y/atspi/accessible/root'


def inside(rect, outer):
    return (len(rect) == len(outer) == 4 and rect[2] > 0 and rect[3] > 0
            and outer[2] > 0 and outer[3] > 0 and rect[0] >= outer[0]
            and rect[1] >= outer[1] and rect[0] + rect[2] <= outer[0] + outer[2]
            and rect[1] + rect[3] <= outer[1] + outer[3])


def intersect(a, b):
    x, y = max(a[0], b[0]), max(a[1], b[1])
    return [x, y, max(0, min(a[0]+a[2], b[0]+b[2])-x),
            max(0, min(a[1]+a[3], b[1]+b[3])-y)]


def state(node, bit):
    return len(node['states']) == 2 and bool(node['states'][bit//32] & (1 << (bit % 32)))


def validate_tree(tree):
    nodes = {n['path']: n for n in tree['nodes']}
    if len(nodes) != len(tree['nodes']) or tree['root'] not in nodes:
        raise RuntimeError('Duplicate or missing accessibility root')
    queue = [(tree['root'], None)]; seen = set()
    while queue:
        path, parent = queue.pop()
        if path in seen or path not in nodes:
            raise RuntimeError('Cyclic, duplicate or missing accessibility edge')
        seen.add(path)
        n = nodes[path]
        if (n['owner'] != tree['owner'] or n['pid'] != tree['pid'] or n['parent'] != parent
                or not re.fullmatch(r'/[A-Za-z0-9_/]+', path)):
            raise RuntimeError('Foreign or disconnected accessibility node')
        queue.extend((p, path) for p in n['children'])
    if len(seen) != len(nodes):
        raise RuntimeError('Disconnected accessibility tree')
    return nodes


def clip_for(node, nodes, window):
    clip = window
    while node is not None:
        # SCROLL_PANE (49), VIEWPORT (68), FRAME (23), DIALOG (16) clip children.
        if node['role'] in (16, 23, 49, 68) and 'rect' in node:
            clip = intersect(clip, node['rect'])
        node = nodes.get(node['parent'])
    return clip


def visible(node, nodes, window):
    return (state(node, 25) and state(node, 30) and COMPONENT in node['interfaces']
            and inside(node.get('rect', []), clip_for(node, nodes, window)))


def mission_content_scope(tree, title, window):
    """Ember's public tree exposes offscreen children below its title bar."""
    nodes=validate_tree(tree)
    frames=[n for n in nodes.values() if n['role']==23 and n['name']==title]
    if len(frames)!=1:
        raise RuntimeError('Missing or ambiguous owned Mission frame')
    frame=frames[0];parent=nodes.get(frame['parent'])
    if (parent is None or parent['role']!=23 or parent['parent']!=tree['root']
            or not inside(frame.get('rect',[]),parent.get('rect',[]))
            or not inside(frame.get('rect',[]),window)):
        raise RuntimeError('Mission frame is not inside the owned top-level application')
    bars=[n for n in nodes.values() if n['role']==67 and n['parent']==frame['path']]
    if len(bars)!=1 or not visible(bars[0],nodes,frame['rect']):
        raise RuntimeError('Missing or ambiguous visible Mission title bar')
    bar=bars[0];x,y,w,h=frame['rect'];top=bar['rect'][1]+bar['rect'][3]
    if not y<top<y+h-8:
        raise RuntimeError('Invalid Mission content viewport')
    return dict(frame=frame['path'],titlebar=bar['path'],content_rect=[x,top,w,y+h-8-top])


def node_observation_bounds(node, nodes, window, mission_scope=None):
    if mission_scope is None:
        return window
    current=node
    while current is not None:
        if current['path']==mission_scope['titlebar']:
            return window  # Close is a real title-bar button, not scrolled content.
        if current['path']==mission_scope['frame']:
            return mission_scope['content_rect']
        current=nodes.get(current['parent'])
    return window


def observe_atspi(pid, window, mission_title=None):
    import dbus
    deadline = time.monotonic() + 25
    def check():
        if time.monotonic() > deadline:
            raise RuntimeError('AT-SPI observation deadline exhausted')
    def iface(bus, owner, path, interface):
        check()
        return dbus.Interface(bus.get_object(owner, path), interface)
    session = dbus.SessionBus()
    address = iface(session, 'org.a11y.Bus', '/org/a11y/bus', 'org.a11y.Bus').GetAddress(timeout=2)
    bus = dbus.bus.BusConnection(str(address))
    broker = iface(bus, 'org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus')
    registry = iface(bus, 'org.a11y.atspi.Registry', ROOT, ACCESSIBLE)
    roots = []
    children = registry.GetChildren(timeout=2)
    if len(children) > 32:
        raise RuntimeError('Private desktop has too many applications')
    for addr in children:
        owner = str(broker.GetNameOwner(str(addr[0]), timeout=2))
        if int(broker.GetConnectionUnixProcessID(owner, timeout=2)) == pid:
            roots.append((owner, str(addr[1])))
    if len(roots) != 1:
        raise RuntimeError('No unique PID-owned accessibility root')
    owner, root = roots[0]
    nodes, seen, queue = [], set(), [(root, None)]
    def children_of(path):
        found = iface(bus, owner, path, ACCESSIBLE).GetChildren(timeout=2)
        if any(str(a[0]) != owner or not re.fullmatch(r'/[A-Za-z0-9_/]+', str(a[1])) for a in found):
            raise RuntimeError('Foreign accessibility edge')
        return [str(a[1]) for a in found]
    while queue:
        check()
        if len(seen) > 2048:
            raise RuntimeError('Accessibility node budget exhausted')
        path, parent = queue.pop(0)
        if path in seen:
            raise RuntimeError('Duplicate accessibility node')
        seen.add(path)
        obj = iface(bus, owner, path, ACCESSIBLE)
        props = iface(bus, owner, path, 'org.freedesktop.DBus.Properties')
        interfaces = list(map(str, obj.GetInterfaces(timeout=2)))
        n = dict(path=path, parent=parent, owner=owner, pid=pid,
                 name=str(props.Get(ACCESSIBLE, 'Name', timeout=2)),
                 role=int(obj.GetRole(timeout=2)), states=list(map(int, obj.GetState(timeout=2))),
                 interfaces=interfaces, children=children_of(path))
        if COMPONENT in interfaces:
            n['rect'] = list(map(int, iface(bus, owner, path, COMPONENT).GetExtents(0, timeout=2)))
        if TEXT in interfaces:
            count = int(props.Get(TEXT, 'CharacterCount', timeout=2))
            if not 0 <= count <= 131072:
                raise RuntimeError('Text bound exceeded')
            n['text'] = str(iface(bus, owner, path, TEXT).GetText(0, count, timeout=2))
        nodes.append(n)
        queue.extend((p, path) for p in n['children'])
    tree = dict(pid=pid, owner=owner, root=root, nodes=nodes)
    by_path = validate_tree(tree)
    mission_scope = mission_content_scope(tree,mission_title,window) if mission_title else None
    if mission_scope:
        tree['mission_content_rect'] = mission_scope['content_rect']
    texts = []
    for n in nodes:
        check()
        bounds = node_observation_bounds(n,by_path,window,mission_scope)
        n['visible'] = visible(n, by_path, bounds)
        n['visible_text'] = ''
        if not state(n, 25) or not state(n, 30):
            continue
        if 'text' in n and 'rect' in n:
            clip = intersect(clip_for(n, by_path, bounds), n['rect'])
            text = n['text']; offset = 0; lines = []
            # Only entire logical lines proven inside the actual clip count.
            # A partially clipped line is not guessed, repaired, or synthesized.
            for line in text.splitlines(keepends=True):
                end = offset + len(line.rstrip('\r\n'))
                if end > offset:
                    rect = iface(bus, owner, n['path'], TEXT).GetRangeExtents(offset, end, 0, timeout=2)
                    if inside(list(map(int, rect)), clip):
                        lines.append(line.rstrip('\r\n'))
                offset += len(line)
            n['visible_text'] = '\n'.join(lines)
        elif n['visible']:
            n['visible_text'] = n['name']
        if n['visible_text']:
            texts.append(n['visible_text'])
    # Reject a traversal spanning any structural update; never repair its tree.
    for n in nodes:
        if children_of(n['path']) != n['children']:
            raise RuntimeError('Accessibility tree changed during observation')
        props = iface(bus, owner, n['path'], 'org.freedesktop.DBus.Properties')
        if str(props.Get(ACCESSIBLE, 'Name', timeout=2)) != n['name']:
            raise RuntimeError('Accessibility name changed during observation')
        if 'text' in n:
            count = int(props.Get(TEXT, 'CharacterCount', timeout=2))
            if count != len(n['text']) or str(iface(bus,owner,n['path'],TEXT).GetText(0,count,timeout=2)) != n['text']:
                raise RuntimeError('Accessibility text changed during observation')
    if int(broker.GetConnectionUnixProcessID(owner, timeout=2)) != pid:
        raise RuntimeError('Accessibility PID changed')
    tree['visible_text'] = '\n'.join(texts)
    return tree


def ocr_rows(raw, scale=2):
    rows = []
    for r in csv.DictReader(io.StringIO(raw), delimiter='\t', quoting=csv.QUOTE_NONE):
        if r.get('level') != '5' or not r.get('text', '').strip():
            continue
        r['conf'] = float(r['conf'])
        for k in ('left', 'top', 'width', 'height'):
            r[k] = round(int(r[k]) / scale)
        rows.append(r)
    return rows


def phrases(rows, text, minimum=80):
    wanted = text.split(); result = []
    if not wanted:
        return result
    for start in range(len(rows)):
        words = rows[start:start+len(wanted)]
        if ([w['text'] for w in words] != wanted or any(w['conf'] < minimum for w in words)
                or len({tuple(w[k] for k in ('page_num', 'block_num', 'par_num', 'line_num')) for w in words}) != 1):
            continue
        x = min(w['left'] for w in words); y = min(w['top'] for w in words)
        result.append([x, y, max(w['left']+w['width'] for w in words)-x,
                       max(w['top']+w['height'] for w in words)-y])
    return result
