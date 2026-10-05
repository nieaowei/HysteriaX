-- Legacy types remain readable for archived migration/audit records only.
ALTER TABLE credentials DROP CONSTRAINT credentials_kind_check;
ALTER TABLE credentials ADD CONSTRAINT credentials_kind_check CHECK (
    kind IN ('ssh_private_key', 'ssh_password', 'tls_identity', 'ca_certificate',
             'certificate', 'private_key', 'ech_key', 'dns', 'api_token')
);
