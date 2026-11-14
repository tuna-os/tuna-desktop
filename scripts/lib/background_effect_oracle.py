"""Assertions over independently captured real compositor background pixels."""

WIDTH, HEIGHT = 384, 256
STEPS = (
    (None, "off"),
    ("on-pending", "on-pending"), ("child-commit", "on-child-cached"), ("parent-commit", "on"),
    ("unmap", "unmapped"), ("remap", "remapped"),
    ("null-pending", "null-pending"), ("child-commit", "null-child-cached"), ("parent-commit", "null-committed"),
    ("on-pending", "destroy-enable-pending"), ("commit", "destroy-on"),
    ("destroy-pending", "destroy-pending"), ("child-commit", "destroy-child-cached"), ("parent-commit", "destroy-committed"),
)


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def locate(image):
    """Find the unique unscaled client border in the real baseline capture."""
    pixels = image.load()
    matches = []
    magenta = (255, 0, 255)
    for y in range(image.height - HEIGHT + 1):
        for x in range(image.width - WIDTH + 1):
            if pixels[x, y] != magenta:
                continue
            if x and pixels[x - 1, y] == magenta:
                continue
            if y and pixels[x, y - 1] == magenta:
                continue
            if all(pixels[x + dx, y + dy] == magenta for dx, dy in (
                (0, 0), (WIDTH - 1, 0), (0, HEIGHT - 1), (WIDTH - 1, HEIGHT - 1),
                (WIDTH // 2, 0), (0, HEIGHT // 2))):
                matches.append((x, y, x + WIDTH, y + HEIGHT))
    require(len(matches) == 1, f"expected one unscaled, unfiltered checkerboard window; found {matches}")
    return matches[0]


def inside(x, y):
    return x < 288 and y < 224 and not (128 <= x < 160 and 96 <= y < 128)


def verify(images, rect):
    """Reusable actual output pixel assertions; raises on any missing effect."""
    crops = {name: image.crop(rect).convert("RGB") for name, image in images.items()}
    sharp = crops["off"]
    require(sharp.size == (WIDTH, HEIGHT), "content capture has wrong dimensions")
    baseline = list(sharp.getdata())
    for y in range(8, HEIGHT - 8):
        for x in range(8, WIDTH - 8):
            expected = (255, 255, 255) if (x // 4 + y // 4) % 2 else (0, 0, 0)
            require(baseline[y * WIDTH + x] == expected, "baseline is not the actual sharp client checkerboard")
    # Pending requests, copied region mutation, synchronized child commits,
    # null and destructor all have independently captured output boundaries.
    unchanged = ("on-pending", "on-child-cached", "unmapped", "null-committed",
                 "destroy-enable-pending", "destroy-committed")
    for name in unchanged:
        require(crops[name].tobytes() == sharp.tobytes(), f"{name}: output differs from OFF")
    blurred = list(crops["on"].getdata())
    changed = outside_changes = core = smoothed = 0
    for y in range(HEIGHT):
        for x in range(WIDTH):
            i = y * WIDTH + x
            different = blurred[i] != baseline[i]
            if inside(x, y):
                changed += different
                if 40 <= x < 272 and 40 <= y < 208:
                    core += 1
                    smoothed += all(80 <= value <= 176 for value in blurred[i])
            else:
                outside_changes += different
    require(outside_changes == 0, f"blur leaks outside clipped/subtracted region: {outside_changes} pixels")
    require(changed > 40000, f"ON does not change the committed region: {changed} pixels")
    require(smoothed / core > .98, f"ON does not smooth checkerboard pixels: {smoothed}/{core}")
    for name in ("remapped", "null-pending", "null-child-cached", "destroy-on", "destroy-pending", "destroy-child-cached"):
        require(crops[name].tobytes() == crops["on"].tobytes(), f"{name}: double-buffered effect changed early")
    return {"changed_region_pixels": changed, "outside_region_changed_pixels": outside_changes,
            "smoothed_core_pixels": smoothed, "core_pixels": core,
            "null_buffer_unmaps_effect": True, "reattach_restores_effect": True,
            "null_restores_off": True, "destroy_restores_off": True,
            "region_copy_negative_clip_subtract": True, "synchronized_subsurface": True}
