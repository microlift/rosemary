"""Binary Ninja loader for Rosemary AOT cache files (`*.rmc`).

`rmc`, `layout` and `context` do not need Binary Ninja, so they can be imported on their own.
"""

try:
    import binaryninja
except ImportError:
    binaryninja = None

if binaryninja is not None:
    from binaryninja import PluginCommand, interaction, log

    from .view import RosemaryAotView, register_settings

    def _view(bv):
        return bv if isinstance(bv, RosemaryAotView) else None

    def _annotate_function(bv, function):
        view = _view(bv)
        found = view.region_at(function.start) if view else None
        if found is None:
            log.log_warn("Rosemary AOT: this function is not a lifted region")
            return
        count = view.annotate_region(*found)
        exits = view.annotate_region_exits(*found)
        log.log_info(f"Rosemary AOT: annotated {count} guest instructions and {exits} exit constants")

    def _find_guest(bv):
        view = _view(bv)
        va = interaction.get_address_input("Guest address", "Find host code for guest address")
        if view is None or va is None:
            return
        found = view.regions_covering(va)
        if not found:
            interaction.show_message_box("Rosemary AOT", f"No cached region covers guest {va:#x}.")
            return
        targets = []
        for c, r in found:
            region = view.cache.chunks[c].regions[r]
            body = view.layout.chunks[c].text + region.link_target
            host = [body + start for start, _end, guest in region.map if view.guest(guest) == va]
            targets.append((f"region_{view.guest(region.lift_base):x}", min(host, default=body)))
        view.navigate(view.view, targets[0][1])
        if len(targets) > 1:
            lines = [f"{name}: host {addr:#x}" for name, addr in targets]
            interaction.show_message_box("Rosemary AOT", "\n".join(lines))

    register_settings()
    RosemaryAotView.register()
    PluginCommand.register_for_function(
        "Rosemary AOT\\Annotate guest addresses and exits",
        "Comment each host instruction run of this region with the guest instruction it came from, "
        "and name the exit reasons and codes it stores",
        _annotate_function,
        lambda bv, function: _view(bv) is not None,
    )
    PluginCommand.register(
        "Rosemary AOT\\Find host code for guest address",
        "Jump to the host code compiled for a guest address",
        _find_guest,
        lambda bv: _view(bv) is not None,
    )
