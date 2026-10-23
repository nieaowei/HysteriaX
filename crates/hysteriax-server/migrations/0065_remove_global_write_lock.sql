-- Maintenance cutover only: old and new service versions must not mix.
-- Retain PostgreSQL resource locks, native constraints and SQLx migration locks.
DO $$
DECLARE entry record;
BEGIN
    FOR entry IN
        SELECT c.relname,t.tgname
        FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid
        JOIN pg_proc p ON p.oid=t.tgfoid
        WHERE c.relnamespace=current_schema()::regnamespace
          AND p.pronamespace=c.relnamespace
          AND p.proname='hysteriax_serialize_writes'
    LOOP
        EXECUTE format('DROP TRIGGER %I ON %I.%I',entry.tgname,current_schema(),entry.relname);
    END LOOP;
    EXECUTE format('DROP FUNCTION IF EXISTS %I.hysteriax_serialize_writes()',current_schema());
END $$;
