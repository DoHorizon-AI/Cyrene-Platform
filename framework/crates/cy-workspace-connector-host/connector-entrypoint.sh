#!/bin/sh
set -eu
umask 077

input_root=/mnt/cyrene-input
private_root=/run/cyrene/workspace-connector
private_secret_root=/run/cyrene/workspace-connector/secrets
product_contract_input_root="$input_root/product-contracts"
product_contract_stage=/run/cyrene-workspace-product-contracts.stage
product_contract_root=/run/cyrene-workspace-product-contracts
service_uid=10001
service_gid=10001

install -d -o "$service_uid" -g "$service_gid" -m 0700 "$private_root"
install -d -o "$service_uid" -g "$service_gid" -m 0700 "$private_secret_root"

stage_required() {
    source_path="$input_root/$1"
    destination_path="$2"
    temporary_path="$destination_path.stage"
    if [ ! -f "$source_path" ] || [ ! -r "$source_path" ]; then
        echo "WORKSPACE_CONNECTOR_INPUT_UNAVAILABLE" >&2
        exit 1
    fi
    rm -f "$temporary_path"
    cat "$source_path" > "$temporary_path"
    chown "$service_uid:$service_gid" "$temporary_path"
    chmod 0600 "$temporary_path"
    mv -f "$temporary_path" "$destination_path"
}

stage_optional_product_secret() {
    secret_name="$1"
    source_path="$input_root/$secret_name"
    if [ -f "$source_path" ] && [ -r "$source_path" ]; then
        stage_required "$secret_name" "$private_secret_root/$secret_name"
    fi
}

stage_product_contracts() {
    if [ ! -d "$product_contract_input_root" ] \
        || [ -L "$product_contract_input_root" ] \
        || [ ! -r "$product_contract_input_root/product-contract-bundle.json" ] \
        || [ -e "$product_contract_stage" ] \
        || [ -L "$product_contract_stage" ] \
        || [ -e "$product_contract_root" ] \
        || [ -L "$product_contract_root" ]; then
        echo "WORKSPACE_CONNECTOR_PRODUCT_CONTRACTS_UNAVAILABLE" >&2
        exit 1
    fi
    if find "$product_contract_input_root" -mindepth 1 \
        ! \( -type d -o -type f \) -print -quit | grep -q .; then
        echo "WORKSPACE_CONNECTOR_PRODUCT_CONTRACTS_INVALID" >&2
        exit 1
    fi

    install -d -o 0 -g 0 -m 0700 "$product_contract_stage"
    cp -R "$product_contract_input_root/." "$product_contract_stage/"
    chown -R 0:0 "$product_contract_stage"
    find "$product_contract_stage" -type d -exec chmod 0555 {} +
    find "$product_contract_stage" -type f -exec chmod 0444 {} +
    mv "$product_contract_stage" "$product_contract_root"
}

stage_required connector.json "$private_root/connector.json"
stage_required product-endpoints.json "$private_root/product-endpoints.json"
stage_required relay-server-ca.pem "$private_secret_root/relay-server-ca.pem"
stage_required device-enrollment-cert.pem "$private_secret_root/device-enrollment-cert.pem"
stage_required device-enrollment-key.pem "$private_secret_root/device-enrollment-key.pem"
stage_product_contracts

stage_optional_product_secret catalyst-token
stage_optional_product_secret yield-token
stage_optional_product_secret reactor-token
stage_optional_product_secret exchange-token
stage_optional_product_secret echo-token
stage_optional_product_secret navigator-token

exec setpriv --reuid="$service_uid" --regid="$service_gid" --clear-groups \
    /usr/local/bin/cy-workspace-connector-host
