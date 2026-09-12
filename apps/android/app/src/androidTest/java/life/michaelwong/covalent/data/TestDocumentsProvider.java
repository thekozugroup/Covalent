package life.michaelwong.covalent.data;

import android.content.Context;
import android.database.Cursor;
import android.database.MatrixCursor;
import android.os.Bundle;
import android.os.CancellationSignal;
import android.os.ParcelFileDescriptor;
import android.provider.DocumentsContract.Document;
import android.provider.DocumentsContract.Root;
import android.provider.DocumentsProvider;

import java.io.File;
import java.io.FileNotFoundException;
import java.io.FileOutputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import java.util.Collections;
import java.util.List;

/** A deterministic test-APK-only SAF provider used through the real ContentResolver boundary. */
public final class TestDocumentsProvider extends DocumentsProvider {
    public static final String AUTHORITY = "life.michaelwong.covalent.test.saf-fixture";
    public static final String METHOD_SET_MODE = "set-mode";
    public static final String MODE_STABLE = "stable";
    public static final String MODE_NULL_CHILD_QUERY = "null-child-query";
    public static final String MODE_THROW_CHILD_QUERY = "throw-child-query";
    public static final String MODE_MUTATE_CHILD_QUERY = "mutate-child-query";

    public static final String ROOT_ID = "root";
    public static final String CHILD_ID = "root/child";
    public static final String CHILD_FILE_ID = "root/child/inside.txt";
    public static final String RECOVERY_KIT_ID = "root/recovery.covalent-recovery";
    public static final String RECOVERY_CODE_ID = "root/recovery-code.txt";
    private static final String ROOT_FILE_ID = "root/top.txt";
    private static final String MUTATED_FILE_ID = "root/appeared.txt";

    private static final String[] DEFAULT_DOCUMENT_PROJECTION = new String[] {
        Document.COLUMN_DOCUMENT_ID,
        Document.COLUMN_DISPLAY_NAME,
        Document.COLUMN_MIME_TYPE,
        Document.COLUMN_FLAGS,
        Document.COLUMN_SIZE,
        Document.COLUMN_LAST_MODIFIED,
    };
    private static final String[] DEFAULT_ROOT_PROJECTION = new String[] {
        Root.COLUMN_ROOT_ID,
        Root.COLUMN_DOCUMENT_ID,
        Root.COLUMN_TITLE,
        Root.COLUMN_FLAGS,
        Root.COLUMN_AVAILABLE_BYTES,
    };

    private String mode = MODE_STABLE;
    private int childQueries;

    @Override
    public boolean onCreate() {
        return true;
    }

    @Override
    public synchronized Bundle call(String method, String argument, Bundle extras) {
        Bundle frameworkResult = super.call(method, argument, extras);
        if (frameworkResult != null || !METHOD_SET_MODE.equals(method)) return frameworkResult;
        mode = argument == null ? MODE_STABLE : argument;
        childQueries = 0;
        recoveryFile(RECOVERY_KIT_ID).delete();
        recoveryFile(RECOVERY_CODE_ID).delete();
        return new Bundle();
    }

    @Override
    public boolean isChildDocument(String parentDocumentId, String documentId) {
        if (ROOT_ID.equals(parentDocumentId)) {
            return CHILD_ID.equals(documentId)
                || CHILD_FILE_ID.equals(documentId)
                || ROOT_FILE_ID.equals(documentId)
                || MUTATED_FILE_ID.equals(documentId)
                || RECOVERY_KIT_ID.equals(documentId)
                || RECOVERY_CODE_ID.equals(documentId);
        }
        return CHILD_ID.equals(parentDocumentId) && CHILD_FILE_ID.equals(documentId);
    }

    @Override
    public Cursor queryRoots(String[] projection) {
        String[] columns = projection == null ? DEFAULT_ROOT_PROJECTION : projection;
        MatrixCursor cursor = new MatrixCursor(columns);
        addRoot(cursor, columns);
        return cursor;
    }

    @Override
    public Cursor queryDocument(String documentId, String[] projection) throws FileNotFoundException {
        requireKnownDocument(documentId);
        return documents(projection, Collections.singletonList(documentId));
    }

    @Override
    public synchronized Cursor queryChildDocuments(
            String parentDocumentId,
            String[] projection,
            String sortOrder
    ) throws FileNotFoundException {
        requireDirectory(parentDocumentId);
        if (ROOT_ID.equals(parentDocumentId)) {
            if (MODE_NULL_CHILD_QUERY.equals(mode)) return null;
            if (MODE_THROW_CHILD_QUERY.equals(mode)) {
                throw new IllegalStateException("Synthetic child query failure");
            }
            if (MODE_MUTATE_CHILD_QUERY.equals(mode)) {
                childQueries += 1;
                return documents(
                    projection,
                    childQueries % 2 == 1
                        ? Collections.singletonList(ROOT_FILE_ID)
                        : Arrays.asList(ROOT_FILE_ID, MUTATED_FILE_ID)
                );
            }
            return documents(projection, Arrays.asList(CHILD_ID, ROOT_FILE_ID));
        }
        return documents(projection, Collections.singletonList(CHILD_FILE_ID));
    }

    @Override
    public ParcelFileDescriptor openDocument(
            String documentId,
            String mode,
            CancellationSignal signal
    ) throws FileNotFoundException {
        requireKnownDocument(documentId);
        if (isDirectory(documentId)) throw new FileNotFoundException("Cannot open a directory");
        if (RECOVERY_KIT_ID.equals(documentId) || RECOVERY_CODE_ID.equals(documentId)) {
            File file = recoveryFile(documentId);
            if (!file.exists()) {
                try {
                    if (!file.createNewFile()) throw new IOException("Could not create recovery fixture");
                } catch (IOException error) {
                    FileNotFoundException failure = new FileNotFoundException("Could not create recovery fixture");
                    failure.initCause(error);
                    throw failure;
                }
            }
            return ParcelFileDescriptor.open(file, ParcelFileDescriptor.parseMode(mode));
        }
        Context context = getContext();
        if (context == null) throw new FileNotFoundException("Fixture provider has no context");
        File file = new File(context.getCacheDir(), "saf-fixture-" + documentId.hashCode());
        try (FileOutputStream output = new FileOutputStream(file, false)) {
            output.write(("fixture:" + documentId).getBytes(StandardCharsets.UTF_8));
        } catch (IOException error) {
            FileNotFoundException failure = new FileNotFoundException("Could not write fixture document");
            failure.initCause(error);
            throw failure;
        }
        return ParcelFileDescriptor.open(file, ParcelFileDescriptor.MODE_READ_ONLY);
    }

    private Cursor documents(String[] projection, List<String> documentIds) {
        String[] columns = projection == null ? DEFAULT_DOCUMENT_PROJECTION : projection;
        MatrixCursor cursor = new MatrixCursor(columns);
        for (String documentId : documentIds) {
            Object[] row = new Object[columns.length];
            for (int index = 0; index < columns.length; index += 1) {
                row[index] = documentValue(documentId, columns[index]);
            }
            cursor.addRow(row);
        }
        return cursor;
    }

    private Object documentValue(String documentId, String column) {
        if (Document.COLUMN_DOCUMENT_ID.equals(column)) return documentId;
        if (Document.COLUMN_DISPLAY_NAME.equals(column)) {
            return ROOT_ID.equals(documentId) ? "Fixture root" : documentId.substring(documentId.lastIndexOf('/') + 1);
        }
        if (Document.COLUMN_MIME_TYPE.equals(column)) {
            return isDirectory(documentId) ? Document.MIME_TYPE_DIR : "application/octet-stream";
        }
        if (Document.COLUMN_FLAGS.equals(column)) {
            return RECOVERY_KIT_ID.equals(documentId) || RECOVERY_CODE_ID.equals(documentId)
                ? Document.FLAG_SUPPORTS_WRITE
                : 0;
        }
        if (Document.COLUMN_SIZE.equals(column)) {
            return isDirectory(documentId) ? 0L : ("fixture:" + documentId).getBytes(StandardCharsets.UTF_8).length;
        }
        if (Document.COLUMN_LAST_MODIFIED.equals(column)) return 1_700_000_000_000L;
        return null;
    }

    private void addRoot(MatrixCursor cursor, String[] columns) {
        Object[] row = new Object[columns.length];
        for (int index = 0; index < columns.length; index += 1) {
            String column = columns[index];
            if (Root.COLUMN_ROOT_ID.equals(column)) row[index] = ROOT_ID;
            else if (Root.COLUMN_DOCUMENT_ID.equals(column)) row[index] = ROOT_ID;
            else if (Root.COLUMN_TITLE.equals(column)) row[index] = "Covalent SAF fixture";
            else if (Root.COLUMN_FLAGS.equals(column)) row[index] = Root.FLAG_SUPPORTS_IS_CHILD;
            else if (Root.COLUMN_AVAILABLE_BYTES.equals(column)) row[index] = 1_024_000L;
        }
        cursor.addRow(row);
    }

    private void requireDirectory(String documentId) throws FileNotFoundException {
        requireKnownDocument(documentId);
        if (!isDirectory(documentId)) throw new FileNotFoundException("Not a directory: " + documentId);
    }

    private void requireKnownDocument(String documentId) throws FileNotFoundException {
        if (!ROOT_ID.equals(documentId)
                && !CHILD_ID.equals(documentId)
                && !ROOT_FILE_ID.equals(documentId)
                && !CHILD_FILE_ID.equals(documentId)
                && !MUTATED_FILE_ID.equals(documentId)
                && !RECOVERY_KIT_ID.equals(documentId)
                && !RECOVERY_CODE_ID.equals(documentId)) {
            throw new FileNotFoundException("Unknown document: " + documentId);
        }
    }

    private boolean isDirectory(String documentId) {
        return ROOT_ID.equals(documentId) || CHILD_ID.equals(documentId);
    }

    private File recoveryFile(String documentId) {
        Context context = getContext();
        if (context == null) throw new IllegalStateException("Fixture provider has no context");
        return new File(context.getCacheDir(), "saf-recovery-" + documentId.hashCode());
    }
}
