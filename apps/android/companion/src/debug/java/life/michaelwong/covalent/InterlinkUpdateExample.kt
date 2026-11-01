package life.michaelwong.covalent

import life.michaelwong.covalent.appliance.*

/** Labelled visual data only. No request, signing or sync operation is performed. */
internal fun interlinkUpdateExample(selected: SelectedAppliance, pending: Boolean = false, enabled: Boolean = false): PiUpdatesState {
    val context = PiUpdateContext(PiImportBond(selected.appliance, selected.local,
        "77777777-7777-4777-8777-777777777777", checkNotNull(selected.confirmedGeneration)), true, enabled,
        "88888888-8888-4888-8888-888888888888", false)
    val choice = PiUpdateChoice(context.bond, "99999999-9999-4999-8999-999999999999", context.revision!!, true)
    return PiUpdatesState(selected, PiUpdatesObservation(selected.confirmedGeneration, true, context),
        if (pending) listOf(PiSavedUpdate(1, choice, choice.canonicalPayload(), "example-only", null, null)) else emptyList(), true)
}
