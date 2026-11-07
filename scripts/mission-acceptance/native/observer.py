"""Bounded, read-only public AT-SPI and screenshot OCR observation.

No Action, EditableText, Selection, Value, focus or mutation interface is used.
Accessible text outside its painted/clipped viewport is retained for diagnosis
but never enters visible_text or supplies an input target.
"""
import csv
import io
import math
import re
import time

ACCESSIBLE = 'org.a11y.atspi.Accessible'
COMPONENT = 'org.a11y.atspi.Component'
TEXT = 'org.a11y.atspi.Text'
ROOT = '/org/a11y/atspi/accessible/root'
NOT_SUPPORTED = 'org.freedesktop.DBus.Error.NotSupported'
# This is an allowlist, not an interface denylist. No mutation API can be
# reached through the observer's proxy, including on future fallback paths.
READ_SIGNATURES = {
    ('org.a11y.Bus', 'GetAddress'): '',
    ('org.freedesktop.DBus', 'GetNameOwner'): 's',
    ('org.freedesktop.DBus', 'GetConnectionUnixProcessID'): 's',
    ('org.freedesktop.DBus.Properties', 'Get'): 'ss',
    (ACCESSIBLE, 'GetChildren'): '', (ACCESSIBLE, 'GetInterfaces'): '',
    (ACCESSIBLE, 'GetRole'): '', (ACCESSIBLE, 'GetState'): '',
    (COMPONENT, 'GetExtents'): 'u', (TEXT, 'GetText'): 'ii',
    (TEXT, 'GetRangeExtents'): 'iiu',
}



class ObservationNotReady(RuntimeError):
    """A proven temporary absence/update, never an ownership or protocol error."""


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


def window_coordinate_anchor(tree, window, binding):
    """Bind zero-SCREEN origins to the caller's observed native client window.

    GTK 4.14.5 returns (0, 0) for SCREEN, not a screen position. WINDOW
    adds accessible-parent bounds; PARENT is only the immediate parent.
    https://github.com/GNOME/gtk/blob/4.14.5/gtk/a11y/gtkatspicomponent.c#L106-L153
    No toolkit-name/path inference, guessed decoration offset or scale is used.
    The caller reads XID/PID/title/client rectangle before capturing and again
    after observation. Only a unique title AND size match can select a client
    in the same PID-owned tree; other native clients never supply evidence.
    """
    nodes = validate_tree(tree)
    components = [n for n in tree['nodes'] if 'rect' in n]
    if not components or any(n['rect'][:2] != [0, 0] for n in components):
        return None
    if (not isinstance(binding, dict) or set(binding) != {'window', 'pid', 'title', 'rect'}
            or type(binding['window']) is not int or binding['window'] <= 0
            or type(binding['pid']) is not int or binding['pid'] != tree['pid']
            or not isinstance(binding['title'], str) or not binding['title']
            or len(binding['title']) > 4096 or '\x00' in binding['title']
            or binding['rect'] != window or len(window) != 4
            or any(type(v) is not int for v in window) or window[2] <= 0 or window[3] <= 0):
        raise RuntimeError('Zero SCREEN coordinates require an observed native client binding')
    root = nodes[tree['root']]
    frames = [nodes[path] for path in root['children']]
    if (root['role'] != 75 or COMPONENT in root['interfaces'] or not frames
            or any(n['role'] != 23 or 'rect' not in n for n in frames)):
        raise RuntimeError('Zero SCREEN coordinates need unambiguous top-level frame ancestry')
    matches = [n for n in frames if n['name'] == binding['title']
               and n['rect'] == [0, 0, *window[2:]] and state(n, 25) and state(n, 30)]
    if len(matches) != 1:
        raise RuntimeError('Missing or ambiguous native client title and size match')
    anchor = matches[0]['path']
    selected = set(); pending = [anchor]
    while pending:
        path = pending.pop(); selected.add(path)
        if nodes[path]['role'] in (16, 69):
            raise RuntimeError('Unsupported nested native dialog/window coordinate boundary')
        pending.extend(nodes[path]['children'])
    return anchor, selected


def project_window_rect(rect, window):
    """Translate actual WINDOW coordinates, without scaling or enlarging clips."""
    return [window[0] + rect[0], window[1] + rect[1], rect[2], rect[3]]


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
    header_rect = [x,y,w,top-y]
    header_close = None
    if title == 'Missions · delivery evidence':
        # Ember/egui exposes Close as a sibling of the title bar, not its
        # descendant. Only this exact direct child can use the header strip.
        closes = [n for n in nodes.values() if n['parent']==frame['path']
                  and n['role']==43 and n['name']=='Close window']
        if len(closes)>1:
            raise RuntimeError('Ambiguous Mission header Close window')
        if closes and visible(closes[0],nodes,header_rect):
            header_close = closes[0]['path']
    return dict(frame=frame['path'],titlebar=bar['path'],header_rect=header_rect,
                header_close=header_close,content_rect=[x,top,w,y+h-8-top])


def node_observation_bounds(node, nodes, window, mission_scope=None):
    if mission_scope is None:
        return window
    if node['path'] == mission_scope.get('header_close'):
        return mission_scope['header_rect']
    current=node
    while current is not None:
        if current['path']==mission_scope['titlebar']:
            return window  # Close is a real title-bar button, not scrolled content.
        if current['path']==mission_scope['frame']:
            return mission_scope['content_rect']
        current=nodes.get(current['parent'])
    return window



def text_geometry_unknown_in_scope(tree, scope, node_paths=None):
    """Unknown text is not an empty/notice-free result.

    Callers must use the actual Mission/status viewport and, when available,
    its descendant paths. An unrelated terminal elsewhere is not a notice.
    Entries retain node identity and clipping provenance for that decision.
    """
    return [entry for entry in tree.get('unknown_text', [])
            if (node_paths is None or entry['path'] in node_paths)
            and all(v > 0 for v in intersect(entry['clip'], scope)[2:])]


def verify_ocr_text_geometry(text, clip, proof, frame_id):
    """Validate actual same-frame SCREEN-coordinate OCR, never invent boxes.

    The callback is given only a node path/clip, not the expected text. Its
    trusted caller must bind it to the captured screenshot identified by
    frame_id. Every supplied word must be high-confidence and wholly inside
    the clip with a pixel of margin. Only exact, unique, complete logical
    lines count; only line-local whitespace is normalized. No substring,
    punctuation repair, cross-line joining or expected-text injection.
    Unobserved lines remain explicitly unknown, never implied invisible.
    """
    if not isinstance(frame_id, str) or not frame_id:
        raise RuntimeError('OCR text geometry requires a captured frame identity')
    if not isinstance(proof, dict) or proof.get('frame_id') != frame_id:
        raise RuntimeError('OCR text geometry belongs to a different frame')
    rows = proof.get('rows')
    if not isinstance(rows, list) or len(rows) > 8192:
        raise RuntimeError('Invalid or excessive OCR text geometry rows')
    expected = [(i, ' '.join(line.split())) for i, line in enumerate(text.splitlines()) if line.strip()]
    empty = dict(status='unavailable', visible_text='', lines=[],
                 unproven_lines=[i for i, _ in expected], reason='No exact complete OCR lines')
    if not rows or clip[2] <= 2 or clip[3] <= 2:
        return empty
    inner = [clip[0]+1, clip[1]+1, clip[2]-2, clip[3]-2]
    grouped = {}
    for row in rows:
        if not isinstance(row, dict):
            raise RuntimeError('Invalid OCR text geometry row')
        word, confidence = row.get('text'), row.get('conf')
        if (not isinstance(word, str) or not word or any(c.isspace() for c in word)
                or type(confidence) not in (int, float) or not math.isfinite(confidence)
                or not 80 <= confidence <= 100):
            return dict(empty, reason='OCR word is missing, malformed or low-confidence')
        rect = [row.get(k) for k in ('left', 'top', 'width', 'height')]
        if any(type(v) is not int for v in rect) or not inside(rect, inner):
            return dict(empty, reason='OCR word is clipped or has invalid geometry')
        key = tuple(row.get(k) for k in ('page_num', 'block_num', 'par_num', 'line_num'))
        if any(not re.fullmatch(r'[1-9][0-9]*', str(v)) for v in key):
            raise RuntimeError('Invalid OCR line identity')
        grouped.setdefault(tuple(map(str, key)), []).append((word, rect, confidence))
    lines = []
    for words in grouped.values():
        words.sort(key=lambda w: w[1][0])
        # A single OCR line must actually be one unambiguous horizontal line.
        top = max(r[1] for _, r, _ in words)
        bottom = min(r[1]+r[3] for _, r, _ in words)
        if top >= bottom or any(a[1][0]+a[1][2] >= b[1][0] for a, b in zip(words, words[1:])):
            return dict(empty, reason='OCR line has overlapping or disjoint word boxes')
        observed = ' '.join(word for word, _, _ in words)
        matches = [i for i, line in expected if line == observed]
        if len(matches) != 1:
            return dict(empty, reason='OCR line is not an exact unique complete text line')
        x = min(r[0] for _, r, _ in words); y = min(r[1] for _, r, _ in words)
        rect = [x, y, max(r[0]+r[2] for _, r, _ in words)-x,
                max(r[1]+r[3] for _, r, _ in words)-y]
        lines.append(dict(index=matches[0], text=observed, rect=rect,
                          words=[dict(text=w, rect=r, conf=c) for w, r, c in words]))
    lines.sort(key=lambda line: (line['rect'][1], line['rect'][0]))
    indices = [line['index'] for line in lines]
    if (len(set(indices)) != len(indices) or indices != sorted(indices)
            or any(a['rect'][1]+a['rect'][3] > b['rect'][1] for a, b in zip(lines, lines[1:]))):
        return dict(empty, reason='OCR logical line order or geometry is ambiguous')
    missing = [i for i, _ in expected if i not in indices]
    return dict(status='ocr_partial' if missing else 'ocr_verified',
                visible_text='\n'.join(line['text'] for line in lines), lines=lines,
                unproven_lines=missing, frame_id=frame_id)


def observe_atspi(pid, window, mission_title=None, *, deadline=None,
                  text_geometry=None, frame_id=None, diagnostics=None, window_binding=None):
    import dbus
    diagnostics = [] if diagnostics is None else diagnostics
    node_context = {}
    def failure(error, owner, path, interface, method, args):
        get_name = getattr(error, 'get_dbus_name', None)
        get_message = getattr(error, 'get_dbus_message', None)
        entry = dict(pid=pid, owner=owner, path=path, interface=interface,
                     method=method, expected_signature=READ_SIGNATURES.get((interface, method)),
                     arguments=[dict(value=int(a) if isinstance(a, int) else str(a),
                                     type=type(a).__name__) for a in args],
                     error_type=type(error).__name__, error_name=get_name() if callable(get_name) else None,
                     error_message=get_message() if callable(get_message) else str(error),
                     node=dict(node_context.get(path, {})))
        diagnostics.append(entry)
        # Keep the original exception class/identity, so no caller can confuse
        # a security, ownership or transport failure with readiness.
        try:
            error.atspi_diagnostics = diagnostics
        except (AttributeError, TypeError):
            pass
    def connect(factory, method, *args):
        try:
            return factory(*args)
        except Exception as error:
            failure(error, None, None, 'connection', method, args)
            raise
    deadline = min(deadline, time.monotonic() + 25) if deadline is not None else time.monotonic() + 25
    def check():
        if time.monotonic() >= deadline:
            raise RuntimeError('AT-SPI observation deadline exhausted')
    def remaining():
        budget = deadline - time.monotonic()
        if budget <= 0:
            raise RuntimeError('AT-SPI observation deadline exhausted')
        return min(2, budget)
    def iface(bus, owner, path, interface):
        check()
        # Do not let implicit introspection introduce an unbounded D-Bus call.
        try:
            endpoint = dbus.Interface(bus.get_object(owner, path, introspect=False), interface)
        except Exception as error:
            failure(error, owner, path, interface, 'Interface', ())
            raise
        class ReadOnlyInterface:
            def __getattr__(self, method):
                if (interface, method) not in READ_SIGNATURES:
                    raise RuntimeError('Forbidden AT-SPI observation method: ' + interface + '.' + method)
                def read(*args, timeout):
                    if interface == 'org.freedesktop.DBus.Properties' and args not in (
                            (ACCESSIBLE, 'Name'), (TEXT, 'CharacterCount')):
                        raise RuntimeError('Forbidden AT-SPI observation property')
                    try:
                        return getattr(endpoint, method)(*args, timeout=timeout)
                    except Exception as error:
                        failure(error, owner, path, interface, method, args)
                        raise
                return read
        return ReadOnlyInterface()
    # With introspection disabled, Python ints default to signed D-Bus 'i'.
    # AT-SPI Component.GetExtents takes 'u'; Text.GetText takes 'ii' and
    # Text.GetRangeExtents takes 'iiu'. SCREEN is coordinate type 0.
    screen = dbus.UInt32(0)
    window_coordinates = dbus.UInt32(1)
    coordinates = screen
    session = connect(dbus.SessionBus, 'SessionBus')
    address = iface(session, 'org.a11y.Bus', '/org/a11y/bus', 'org.a11y.Bus').GetAddress(timeout=remaining())
    bus = connect(dbus.bus.BusConnection, 'BusConnection', str(address))
    broker = iface(bus, 'org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus')
    registry = iface(bus, 'org.a11y.atspi.Registry', ROOT, ACCESSIBLE)
    def owned_root():
        roots = []
        children = registry.GetChildren(timeout=remaining())
        if len(children) > 32:
            raise RuntimeError('Private desktop has too many applications')
        for addr in children:
            owner = str(broker.GetNameOwner(str(addr[0]), timeout=remaining()))
            if int(broker.GetConnectionUnixProcessID(owner, timeout=remaining())) == pid:
                roots.append((owner, str(addr[1])))
        if not roots:
            raise ObservationNotReady('PID-owned accessibility root is not registered yet')
        if len(roots) != 1:
            raise RuntimeError('Ambiguous PID-owned accessibility roots')
        return roots[0]
    owner, root = owned_root()
    def checked_rect(raw, method):
        # The wire result is (iiii), not arbitrary numeric/coercible values.
        # Exact types also reject dbus.Boolean/UInt32, both int subclasses.
        if (not isinstance(raw, (tuple, list)) or len(raw) != 4
                or any(type(v) not in (int, dbus.Int32)
                       or not -(1 << 31) <= v < (1 << 31) for v in raw)
                or raw[2] < 0 or raw[3] < 0):
            raise RuntimeError('Malformed AT-SPI ' + method + ' extents')
        return list(map(int, raw))
    def component_rect(path, coordinate_type):
        raw = iface(bus, owner, path, COMPONENT).GetExtents(coordinate_type, timeout=remaining())
        return checked_rect(raw, 'Component')
    nodes, seen, queue = [], set(), [(root, None)]
    def children_of(path):
        found = iface(bus, owner, path, ACCESSIBLE).GetChildren(timeout=remaining())
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
        interfaces = list(map(str, obj.GetInterfaces(timeout=remaining())))
        node_context[path] = dict(path=path, parent=parent, interfaces=interfaces)
        n = dict(path=path, parent=parent, owner=owner, pid=pid, interfaces=interfaces)
        n['name'] = str(props.Get(ACCESSIBLE, 'Name', timeout=remaining()))
        node_context[path]['name'] = n['name']
        n['role'] = int(obj.GetRole(timeout=remaining()))
        node_context[path]['role'] = n['role']
        n['states'] = list(map(int, obj.GetState(timeout=remaining())))
        n['children'] = children_of(path)
        if COMPONENT in interfaces:
            n['rect'] = component_rect(path, screen)
        if TEXT in interfaces:
            count = int(props.Get(TEXT, 'CharacterCount', timeout=remaining()))
            if not 0 <= count <= 131072:
                raise RuntimeError('Text bound exceeded')
            n['text'] = str(iface(bus, owner, path, TEXT).GetText(dbus.Int32(0), dbus.Int32(count), timeout=remaining()))
        nodes.append(n)
        queue.extend((p, path) for p in n['children'])
    tree = dict(pid=pid, owner=owner, root=root, nodes=nodes)
    by_path = validate_tree(tree)
    selection = window_coordinate_anchor(tree, window, window_binding)
    anchor = selection[0] if selection is not None else None
    if selection is not None:
        coordinates = window_coordinates
        for n in nodes:
            n['coordinate_selected'] = n['path'] in selection[1]
            if 'rect' not in n:
                continue
            raw = component_rect(n['path'], coordinates)
            if raw[2:] != n['rect'][2:]:
                raise RuntimeError('SCREEN and WINDOW component dimensions differ')
            if n['path'] == anchor and raw != [0, 0, *window[2:]]:
                raise RuntimeError('WINDOW frame origin or size does not match captured native client')
            n['component_extents'] = dict(screen=n['rect'][:], window=raw[:])
            if n['coordinate_selected']:
                n['rect'] = project_window_rect(raw, window)
            else:
                # Raw rectangles from another native client have a different
                # origin. Retain them for diagnostics, never as screen evidence.
                del n['rect']
        tree['coordinate_geometry'] = dict(mode='window_to_screen', anchor=anchor,
            captured_window=window[:], offset=window[:2], binding=dict(window_binding))
    mission_scope = mission_content_scope(tree,mission_title,window) if mission_title else None
    if mission_scope:
        tree['mission_content_rect'] = mission_scope['content_rect']
        tree['mission_header_rect'] = mission_scope['header_rect']
        tree['mission_header_close'] = mission_scope['header_close']
    texts = []
    tree['unknown_text'] = []
    unsupported_seen = False
    for n in nodes:
        check()
        bounds = node_observation_bounds(n,by_path,window,mission_scope)
        n['visible'] = visible(n, by_path, bounds)
        n['visible_text'] = ''
        if n.get('coordinate_selected') is False or not state(n, 25) or not state(n, 30):
            continue
        if 'text' in n and 'rect' in n:
            clip = intersect(clip_for(n, by_path, bounds), n['rect'])
            # No pixels in this owned viewport means there is no text geometry
            # to query or unknown notice to infer. Do not probe offscreen text.
            if clip[2] <= 0 or clip[3] <= 0:
                n['text_geometry_status'] = 'outside_clip'
                continue
            text = n['text']; offset = 0; lines = []
            # Only entire logical lines proven inside the actual clip count.
            # A partially clipped line is not guessed, repaired, or synthesized.
            for line in text.splitlines(keepends=True):
                end = offset + len(line.rstrip('\r\n'))
                if end > offset:
                    range_text = iface(bus, owner, n['path'], TEXT)
                    try:
                        rect = range_text.GetRangeExtents(
                            dbus.Int32(offset), dbus.Int32(end), coordinates, timeout=remaining())
                    except Exception as error:
                        dbus_error = getattr(dbus, 'DBusException', ())
                        if not (isinstance(error, dbus_error) and error.get_dbus_name() == NOT_SUPPORTED
                                and TEXT in n['interfaces']):
                            raise
                        unsupported_seen = True
                        unsupported = diagnostics[-1]
                        # Only this exact optional geometry operation may be
                        # unavailable. No name/widget rectangle becomes text
                        # proof, and unsupported is never a readiness retry.
                        geometry = dict(status='unavailable', visible_text='', lines=[],
                            unproven_lines=[i for i, value in enumerate(text.splitlines()) if value],
                            reason='Text.GetRangeExtents is not supported')
                        if clip[2] <= 0 or clip[3] <= 0:
                            geometry = dict(geometry, status='outside_clip', unproven_lines=[])
                        elif text_geometry is not None:
                            check()
                            proof = text_geometry(n['path'], clip[:], deadline=deadline)
                            check()
                            geometry = verify_ocr_text_geometry(text, clip, proof, frame_id)
                        n['text_geometry'] = dict(geometry, clip=clip[:],
                            unsupported=unsupported)
                        n['text_geometry_status'] = ('unknown' if geometry['unproven_lines']
                                                     else geometry['status'])
                        lines = geometry['visible_text'].splitlines()
                        if geometry['unproven_lines']:
                            tree['unknown_text'].append(dict(path=n['path'], owner=owner,
                                pid=pid, role=n['role'], clip=clip[:], rect=n['rect'][:],
                                status=geometry['status'], unproven_lines=geometry['unproven_lines']))
                        break
                    rect = checked_rect(rect, 'Text range')
                    if anchor is not None:
                        rect = project_window_rect(rect, window)
                    if inside(rect, clip):
                        lines.append(line.rstrip('\r\n'))
                offset += len(line)
            n['visible_text'] = '\n'.join(lines)
        elif n['visible']:
            n['visible_text'] = n['name']
        if n['visible_text']:
            texts.append(n['visible_text'])
    # Re-read the complete structure, including new edges, before classifying
    # a change as temporary. Foreign edges, duplicates/cycles and D-Bus errors
    # are hard failures even when another part of the tree changed first.
    latest, seen, queue = [], set(), [(root, None)]
    while queue:
        check()
        if len(seen) > 2048:
            raise RuntimeError('Accessibility node budget exhausted')
        path, parent = queue.pop(0)
        if path in seen:
            raise RuntimeError('Duplicate accessibility node')
        seen.add(path)
        n = dict(path=path, parent=parent, owner=owner, pid=pid, children=children_of(path))
        latest.append(n)
        queue.extend((p, path) for p in n['children'])
    latest_by_path = validate_tree(dict(pid=pid, owner=owner, root=root, nodes=latest))
    changes = []
    if (set(latest_by_path) != set(by_path)
            or any(n['children'] != by_path[n['path']]['children'] for n in latest if n['path'] in by_path)):
        changes.append('tree')
    for n in nodes:
        props = iface(bus, owner, n['path'], 'org.freedesktop.DBus.Properties')
        if str(props.Get(ACCESSIBLE, 'Name', timeout=remaining())) != n['name']:
            changes.append('name')
        if unsupported_seen or anchor is not None:
            # OCR boxes belong to the captured clip. Verify node geometry and
            # all clipping ancestors, not just text, before accepting them.
            obj = iface(bus, owner, n['path'], ACCESSIBLE)
            if (list(map(str, obj.GetInterfaces(timeout=remaining()))) != n['interfaces']
                    or int(obj.GetRole(timeout=remaining())) != n['role']
                    or list(map(int, obj.GetState(timeout=remaining()))) != n['states']):
                changes.append('node properties')
            if 'rect' in n or 'component_extents' in n:
                original = n.get('component_extents')
                expected = original['screen'] if original else n['rect']
                if component_rect(n['path'], screen) != expected:
                    changes.append('geometry')
                if original is not None and component_rect(n['path'], window_coordinates) != original['window']:
                    changes.append('WINDOW geometry')
        if 'text' in n:
            count = int(props.Get(TEXT, 'CharacterCount', timeout=remaining()))
            if not 0 <= count <= 131072:
                raise RuntimeError('Text bound exceeded')
            if count != len(n['text']) or str(iface(bus,owner,n['path'],TEXT).GetText(
                    dbus.Int32(0),dbus.Int32(count),timeout=remaining())) != n['text']:
                changes.append('text')
    if int(broker.GetConnectionUnixProcessID(owner, timeout=remaining())) != pid:
        raise RuntimeError('Accessibility PID changed')
    if owned_root() != (owner, root):
        raise RuntimeError('Accessibility root or owner changed')
    check()
    if changes:
        # Never pair this updated tree with the earlier screenshot. The caller
        # must discard this attempt and capture/recheck a whole new frame.
        raise ObservationNotReady('Accessibility %s changed during observation' % '/'.join(dict.fromkeys(changes)))
    tree['visible_text'] = '\n'.join(texts)
    tree['atspi_diagnostics'] = diagnostics
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
