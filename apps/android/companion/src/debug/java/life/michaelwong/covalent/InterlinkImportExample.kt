package life.michaelwong.covalent

import life.michaelwong.covalent.appliance.*

/** Rendering-only example. No device, private key, RPC or actual import exists. */
internal fun interlinkImportExample(selected: SelectedAppliance): PiImportReview {
    val source = PiImportControlSource(PiImportSource("a".repeat(64), false, "Camera SD card", true, false,
        "DCIM/GPhoto/Imports/912666d028529101", PiPreviewState.READY, 320, 1_400_000_000),
        "66666666-6666-4666-8666-666666666666", 1,
        PiImportConfirmation("77777777-7777-4777-8777-777777777777", "88888888-8888-4888-8888-888888888888"))
    val bond = PiImportBond(selected.appliance, selected.local, "55555555-5555-4555-8555-555555555555", checkNotNull(selected.confirmedGeneration))
    return PiImportReview(selected, PiImportContext(bond, true, false, listOf(source), 8_500_000_000), source, false,
        "99999999-9999-4999-8999-999999999999")
}
