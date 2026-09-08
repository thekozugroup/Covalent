package life.michaelwong.covalent.sync;

import android.database.Cursor;
import android.database.MatrixCursor;
import android.os.Bundle;
import android.os.CancellationSignal;
import android.os.ParcelFileDescriptor;
import android.provider.DocumentsContract.Document;
import android.provider.DocumentsContract.Root;
import android.provider.DocumentsProvider;

import java.io.FileNotFoundException;
import java.util.Arrays;
import java.util.Collections;
import java.util.List;

/** Test-APK-only provider for strict sync inventory tests through ContentResolver. */
public final class SyncTestDocumentsProvider extends DocumentsProvider {
    public static final String AUTHORITY = "life.michaelwong.covalent.test.sync-saf-fixture";
    public static final String METHOD_SET_MODE = "set-sync-mode";
    public static final String MODE_STABLE = "stable";
    public static final String MODE_NULL = "null";
    public static final String MODE_LOADING = "loading";
    public static final String MODE_ERROR = "error";
    public static final String MODE_THROW_AFTER_ROW = "throw-after-row";
    public static final String MODE_PARTIAL = "partial";
    public static final String MODE_MUTATE = "mutate";
    public static final String MODE_CYCLE = "cycle";
    public static final String MODE_DUPLICATE = "duplicate";
    public static final String MODE_NAME_COLLISION = "name-collision";

    public static final String ROOT_ID = "opaque:/root?grant=unchanged";
    public static final String DIRECTORY_ID = "opaque:/directory#one";
    public static final String ROOT_FILE_ID = "opaque:/file?one";
    public static final String NESTED_FILE_ID = "opaque:/nested/file:two";
    private static final String MUTATED_FILE_ID = "opaque:/appeared";

    private static final String[] DOCUMENT_PROJECTION = new String[] {
        Document.COLUMN_DOCUMENT_ID,
        Document.COLUMN_DISPLAY_NAME,
        Document.COLUMN_MIME_TYPE,
        Document.COLUMN_FLAGS,
        Document.COLUMN_SIZE,
        Document.COLUMN_LAST_MODIFIED,
    };
    private static final String[] ROOT_PROJECTION = new String[] {
        Root.COLUMN_ROOT_ID,
        Root.COLUMN_DOCUMENT_ID,
        Root.COLUMN_TITLE,
        Root.COLUMN_FLAGS,
    };

    private String mode = MODE_STABLE;
    private int rootChildQueries;

    @Override
    public boolean onCreate() {
        return true;
    }

    @Override
    public synchronized Bundle call(String method, String argument, Bundle extras) {
        Bundle frameworkResult = super.call(method, argument, extras);
        if (frameworkResult != null || !METHOD_SET_MODE.equals(method)) return frameworkResult;
        mode = argument == null ? MODE_STABLE : argument;
        rootChildQueries = 0;
        return new Bundle();
    }

    @Override
    public boolean isChildDocument(String parentDocumentId, String documentId) {
        if (ROOT_ID.equals(parentDocumentId)) {
            return DIRECTORY_ID.equals(documentId)
                || ROOT_FILE_ID.equals(documentId)
                || NESTED_FILE_ID.equals(documentId)
                || MUTATED_FILE_ID.equals(documentId);
        }
        return DIRECTORY_ID.equals(parentDocumentId) && NESTED_FILE_ID.equals(documentId);
    }

    @Override
    public Cursor queryRoots(String[] projection) {
        String[] columns = projection == null ? ROOT_PROJECTION : projection;
        MatrixCursor cursor = new MatrixCursor(columns);
        Object[] row = new Object[columns.length];
        for (int index = 0; index < columns.length; index += 1) {
            String column = columns[index];
            if (Root.COLUMN_ROOT_ID.equals(column)) row[index] = ROOT_ID;
            else if (Root.COLUMN_DOCUMENT_ID.equals(column)) row[index] = ROOT_ID;
            else if (Root.COLUMN_TITLE.equals(column)) row[index] = "Sync inventory fixture";
            else if (Root.COLUMN_FLAGS.equals(column)) row[index] = Root.FLAG_SUPPORTS_IS_CHILD;
        }
        cursor.addRow(row);
        return cursor;
    }

    @Override
    public Cursor queryDocument(String documentId, String[] projection) throws FileNotFoundException {
        requireKnown(documentId);
        return documents(projection, Collections.singletonList(row(documentId, null, 0)));
    }

    @Override
    public synchronized Cursor queryChildDocuments(
            String parentDocumentId,
            String[] projection,
            String sortOrder
    ) throws FileNotFoundException {
        requireDirectory(parentDocumentId);
        if (!ROOT_ID.equals(parentDocumentId)) {
            return documents(projection, Collections.singletonList(row(NESTED_FILE_ID, "inside.txt", 0)));
        }

        rootChildQueries += 1;
        if (MODE_NULL.equals(mode)) return null;
        if (MODE_LOADING.equals(mode)) {
            Bundle extras = new Bundle();
            extras.putBoolean(android.provider.DocumentsContract.EXTRA_LOADING, true);
            return documents(projection, Collections.emptyList(), extras, false);
        }
        if (MODE_ERROR.equals(mode)) {
            Bundle extras = new Bundle();
            extras.putString(android.provider.DocumentsContract.EXTRA_ERROR, "private provider detail");
            return documents(projection, Collections.emptyList(), extras, false);
        }
        if (MODE_THROW_AFTER_ROW.equals(mode)) {
            return documents(
                projection,
                Arrays.asList(row(ROOT_FILE_ID, "top.txt", 0), row(MUTATED_FILE_ID, "second.txt", 0)),
                Bundle.EMPTY,
                true
            );
        }
        if (MODE_PARTIAL.equals(mode)) {
            return documents(
                projection,
                Collections.singletonList(row(ROOT_FILE_ID, "top.txt", Document.FLAG_PARTIAL))
            );
        }
        if (MODE_MUTATE.equals(mode) && rootChildQueries >= 2) {
            return documents(
                projection,
                Arrays.asList(row(ROOT_FILE_ID, "top.txt", 0), row(MUTATED_FILE_ID, "appeared.txt", 0))
            );
        }
        if (MODE_CYCLE.equals(mode)) {
            return documents(projection, Collections.singletonList(row(ROOT_ID, "again", 0)));
        }
        if (MODE_DUPLICATE.equals(mode)) {
            return documents(
                projection,
                Arrays.asList(row(ROOT_FILE_ID, "one.txt", 0), row(ROOT_FILE_ID, "two.txt", 0))
            );
        }
        if (MODE_NAME_COLLISION.equals(mode)) {
            return documents(
                projection,
                Arrays.asList(row(ROOT_FILE_ID, "Report.txt", 0), row(MUTATED_FILE_ID, "report.TXT", 0))
            );
        }
        return documents(
            projection,
            Arrays.asList(row(DIRECTORY_ID, "Folder", 0), row(ROOT_FILE_ID, "top.txt", 0))
        );
    }

    @Override
    public ParcelFileDescriptor openDocument(
            String documentId,
            String mode,
            CancellationSignal signal
    ) throws FileNotFoundException {
        throw new FileNotFoundException("Metadata-only fixture does not open content");
    }

    private Cursor documents(String[] projection, List<Row> rows) {
        return documents(projection, rows, Bundle.EMPTY, false);
    }

    private Cursor documents(
            String[] projection,
            List<Row> rows,
            Bundle extras,
            boolean failAfterFirst
    ) {
        String[] columns = projection == null ? DOCUMENT_PROJECTION : projection;
        MatrixCursor cursor = new FixtureCursor(columns, extras, failAfterFirst);
        for (Row document : rows) {
            Object[] values = new Object[columns.length];
            for (int index = 0; index < columns.length; index += 1) {
                values[index] = value(document, columns[index]);
            }
            cursor.addRow(values);
        }
        return cursor;
    }

    private Object value(Row row, String column) {
        if (Document.COLUMN_DOCUMENT_ID.equals(column)) return row.id;
        if (Document.COLUMN_DISPLAY_NAME.equals(column)) return row.name;
        if (Document.COLUMN_MIME_TYPE.equals(column)) {
            return isDirectory(row.id) ? Document.MIME_TYPE_DIR : "application/octet-stream";
        }
        if (Document.COLUMN_FLAGS.equals(column)) return row.flags;
        if (Document.COLUMN_SIZE.equals(column)) return isDirectory(row.id) ? null : 11L;
        if (Document.COLUMN_LAST_MODIFIED.equals(column)) return 1_700_000_000_000L;
        return null;
    }

    private Row row(String id, String displayName, int flags) {
        String name = displayName;
        if (name == null) name = ROOT_ID.equals(id) ? "Fixture root" : "document";
        return new Row(id, name, flags);
    }

    private void requireDirectory(String documentId) throws FileNotFoundException {
        requireKnown(documentId);
        if (!isDirectory(documentId)) throw new FileNotFoundException("Not a directory");
    }

    private void requireKnown(String documentId) throws FileNotFoundException {
        if (!ROOT_ID.equals(documentId)
                && !DIRECTORY_ID.equals(documentId)
                && !ROOT_FILE_ID.equals(documentId)
                && !NESTED_FILE_ID.equals(documentId)
                && !MUTATED_FILE_ID.equals(documentId)) {
            throw new FileNotFoundException("Unknown document");
        }
    }

    private boolean isDirectory(String documentId) {
        return ROOT_ID.equals(documentId) || DIRECTORY_ID.equals(documentId);
    }

    private static final class Row {
        final String id;
        final String name;
        final int flags;

        Row(String id, String name, int flags) {
            this.id = id;
            this.name = name;
            this.flags = flags;
        }
    }

    private static final class FixtureCursor extends MatrixCursor {
        private final Bundle extras;
        private final boolean failAfterFirst;

        FixtureCursor(String[] columns, Bundle extras, boolean failAfterFirst) {
            super(columns);
            this.extras = extras;
            this.failAfterFirst = failAfterFirst;
        }

        @Override
        public Bundle getExtras() {
            return extras;
        }

        @Override
        public boolean onMove(int oldPosition, int newPosition) {
            if (failAfterFirst && newPosition > 0) {
                throw new IllegalStateException("Synthetic partial cursor failure");
            }
            return super.onMove(oldPosition, newPosition);
        }
    }
}
