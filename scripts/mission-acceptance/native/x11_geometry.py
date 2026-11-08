"""Read-only X11 client geometry. Never compensate for a guessed WM offset."""
import re


def require(test, message):
    if not test:
        raise RuntimeError(message)


def xwininfo_record(raw, children=False):
    """Parse each security-relevant xwininfo field exactly once under LC_ALL=C."""
    require(isinstance(raw, str) and len(raw) <= 131072, 'Invalid X11 geometry output')
    patterns = {
        'window': r'xwininfo: Window id: ([1-9][0-9]*)(?: .*)?',
        'x': r'  Absolute upper-left X:  (-?[0-9]+)',
        'y': r'  Absolute upper-left Y:  (-?[0-9]+)',
        'relative_x': r'  Relative upper-left X:  (-?[0-9]+)',
        'relative_y': r'  Relative upper-left Y:  (-?[0-9]+)',
        'width': r'  Width: ([0-9]+)',
        'height': r'  Height: ([0-9]+)',
        'border': r'  Border width: ([0-9]+)',
        'class': r'  Class: (.+)',
        'map': r'  Map State: (.+)',
    }
    if children:
        patterns.update(root=r'  Root window id: ([1-9][0-9]*)(?: .*)?',
                        parent=r'  Parent window id: ([1-9][0-9]*)(?: .*)?')
    prefixes = dict(window='xwininfo:', x='Absolute upper-left X:',
                    y='Absolute upper-left Y:', relative_x='Relative upper-left X:',
                    relative_y='Relative upper-left Y:', width='Width:', height='Height:',
                    border='Border width:', **{'class':'Class:', 'map':'Map State:',
                    'root':'Root window id:', 'parent':'Parent window id:'})
    record = {}
    for key, pattern in patterns.items():
        # Count candidate field lines before validating the value. An extra
        # malformed field cannot disappear from a successful findall result.
        lines = [line for line in raw.splitlines() if line.lstrip().startswith(prefixes[key])]
        require(len(lines) == 1, 'Missing or duplicate X11 geometry field: '+key)
        match = re.fullmatch(pattern, lines[0])
        require(match is not None, 'Malformed X11 geometry field: '+key)
        value = match[1]
        record[key] = value if key in ('class','map') else int(value)
    require(all(0 < record[key] <= 0xffffffff for key in ('window', 'root', 'parent') if key in record),
            'X11 window identity outside protocol range')
    require(0 <= record['border'] <= 65535
            and all(-32768 <= record[key] <= 32767 for key in ('x','y','relative_x','relative_y')),
            'X11 coordinate or border outside protocol range')
    require(0 < record['width'] <= 65535 and 0 < record['height'] <= 65535
            and record['class'] == 'InputOutput' and record['map'] == 'IsViewable',
            'X11 client is not a positive-size viewable InputOutput window')
    return record


def client_geometry(raw, root_raw, wid, legacy_rect):
    """xwininfo absolute origin is border exterior; AT-SPI WINDOW uses client interior."""
    current, root = xwininfo_record(raw, children=True), xwininfo_record(root_raw)
    require(current['window'] == int(wid), 'X11 geometry belongs to another window')
    require(current['root'] == root['window'] and current['window'] != root['window'],
            'X11 client belongs to another captured root')
    require(root['x'] == root['y'] == root['border'] == root['relative_x'] == root['relative_y'] == 0,
            'Unexpected captured X11 root origin or border')
    require([current['width'],current['height']] == list(legacy_rect[2:]),
            'X11 and xdotool client dimensions differ')
    # xwininfo uses XTranslateCoordinates(client, root, -border, -border).
    # Add only the measured border, never attr.x/y or a fixed decoration size.
    result = [current['x']+current['border'], current['y']+current['border'],
              current['width'], current['height']]
    x,y,w,h = result
    require(x >= 0 and y >= 0 and x+w <= root['width'] and y+h <= root['height'],
            'X11 client is not fully inside captured root')
    return result, dict(client=current, root=root, xdotool_rect=list(legacy_rect),
                        basis='xwininfo_absolute_plus_measured_border')
