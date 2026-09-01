package fixture

internal class DurableTransferJournalFixture {
    fun validateBackup(path: String) {
        require(path == "/api/v1/backups/archive-unknown")
    }

    fun validateRestore(path: String) {
        require(path == "/api/v1/restores/archive/execute")
    }
}
