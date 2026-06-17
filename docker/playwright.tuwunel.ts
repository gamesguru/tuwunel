import {
    AbstractStartedContainer,
    GenericContainer,
    type StartedTestContainer,
    Wait,
} from "testcontainers";
import {type APIRequestContext, type TestInfo} from "@playwright/test";
import crypto from "node:crypto";
import * as net from "node:net";

import {
    type HomeserverContainer,
    type StartedHomeserverContainer,
    type StartedMatrixAuthenticationServiceContainer,
} from "@element-hq/element-web-playwright-common/lib/testcontainers/index.js";
import {ClientServerApi, type Credentials} from "@element-hq/element-web-playwright-common/lib/utils/api.js";

// Matches the `registration_shared_secret` baked into the testee image at
// /playwright/playwright.toml. The endpoint accepts any value, but the testee
// rejects mismatches with a 403, so this constant is load-bearing.
const REGISTRATION_SHARED_SECRET = "playwright-shared-secret";

// The minimum config surface tests are allowed to flip at runtime. The full
// config lives in tuwunel.toml; the in-image baked file is the source of
// truth. withConfigField/withConfig accept any key, but only the keys listed
// here are forwarded as -O flags at startup.
const DEFAULT_CONFIG = {
    log: "warn,tuwunel=info",
};
export type TuwunelConfig = typeof DEFAULT_CONFIG;

/**
 * A Playwright homeserver container backed by Tuwunel.
 *
 * Warning: this is an unstable API/interface and may change without notice.
 */
export class TuwunelContainer extends GenericContainer implements HomeserverContainer<TuwunelConfig> {
    protected config: TuwunelConfig;
    private cmdOverrides: string[] = [];

    /**
     * Creates a new TuwunelContainer.
     * @param image The image tag to start from. Defaults to the locally-built testee image.
     */
    public constructor(image = "tuwunel-playwright-testee:latest") {
        super(image);

        this.config = {...DEFAULT_CONFIG};

        this.withWaitStrategy(Wait.forHttp("/_matrix/client/versions", 8008))
            .withEnvironment({SERVER_NAME: "localhost"});
    }

    /**
     * Overrides a single config field for this container.
     * @param key The config key to override.
     * @param value The value to set.
     * @returns This container, for chaining.
     */
    public withConfigField<Key extends keyof TuwunelConfig>(key: Key, value: TuwunelConfig[Key]): this {
        (this.config as Record<string, unknown>)[key as string] = value;
        this.cmdOverrides.push(`-O${String(key)}=${String(value)}`);
        return this;
    }

    /**
     * Overrides multiple config fields for this container.
     * @param config A partial config to merge over the defaults.
     * @returns This container, for chaining.
     */
    public withConfig(config: Partial<TuwunelConfig>): this {
        this.config = {...this.config, ...config};
        for (const [k, v] of Object.entries(config)) {
            this.cmdOverrides.push(`-O${k}=${String(v)}`);
        }
        return this;
    }

    /**
     * No-op: the tuwunel test harness does not yet ship an SMTP server.
     * Specs that require email verification must be on the skip-list.
     * @returns This container, for chaining.
     */
    public withSmtpServer(): this {
        return this; // XXX: SMTP support not implemented
    }

    /**
     * No-op: tuwunel does not integrate with MAS in the test harness.
     * Specs that require MAS must be on the skip-list.
     * @returns This container, for chaining.
     */
    public withMatrixAuthenticationService(_mas?: StartedMatrixAuthenticationServiceContainer): this {
        return this; // XXX: MAS integration not implemented
    }

    public override async start(): Promise<StartedTuwunelContainer> {
        if (this.cmdOverrides.length > 0) {
            this.withCommand(this.cmdOverrides);
        }
        // Pin the host port so baseUrl survives homeserver.restart() (the spotlight
        // suite's context fixture restarts the testee between tests). 127.0.0.1 over
        // testcontainers' default getHost() to avoid the docker0 bridge IP, which
        // ECONNREFUSEs from Playwright's apiRequestContext under --network=host.
        //
        // pickFreePort releases its probe socket before the daemon binds the
        // port, so a concurrent worker can claim it in the gap (workers run
        // several testees at once). Retry with a fresh pick on the daemon's
        // "port is already allocated" until one sticks.
        for (let attempt = 1; ; attempt++) {
            const port = await pickFreePort();
            this.withExposedPorts({container: 8008, host: port});
            try {
                const container = await super.start();
                const baseUrl = `http://127.0.0.1:${port}`;
                return new StartedTuwunelContainer(container, baseUrl, REGISTRATION_SHARED_SECRET);
            } catch (e) {
                if (attempt >= 5 || !/port is already allocated|address already in use/i.test(String(e))) {
                    throw e;
                }
            }
        }
    }
}

async function pickFreePort(): Promise<number> {
    return new Promise<number>((resolve) => {
        const srv = net.createServer();
        srv.listen(0, () => {
            const port = (srv.address() as net.AddressInfo).port;
            srv.close(() => resolve(port));
        });
    });
}

/**
 * A started TuwunelContainer, exposing the Client-Server API and a small
 * Synapse-compatible admin surface for test registration.
 */
export class StartedTuwunelContainer extends AbstractStartedContainer implements StartedHomeserverContainer {
    public readonly csApi: ClientServerApi;

    public constructor(
        container: StartedTestContainer,
        public readonly baseUrl: string,
        private readonly registrationSharedSecret: string,
    ) {
        super(container);
        this.csApi = new ClientServerApi(this.baseUrl);
    }

    public setRequest(request: APIRequestContext): void {
        this.csApi.setRequest(request);
    }

    public async onTestFinished(_testInfo: TestInfo): Promise<void> {
        // XXX: per-spec cleanup (publicRooms hide, user purge) is M3 work
    }

    private requestContext(): APIRequestContext {
        // XXX: ClientServerApi doesn't expose its request context publicly
        const ctx = (this.csApi as unknown as {_request?: APIRequestContext})._request;
        if (!ctx) {
            throw new Error("No request context; call setRequest first");
        }
        return ctx;
    }

    /**
     * Registers a user via the Synapse-compatible admin register endpoint.
     * @param username The localpart of the user to register.
     * @param password The password to set.
     * @param displayName The optional display name to set.
     * @param admin Whether the user should be granted admin privileges.
     * @returns The credentials of the newly-registered user.
     * @throws Error If the nonce GET or register POST fails.
     */
    private async registerUserInternal(
        username: string,
        password: string,
        displayName: string | undefined,
        admin: boolean,
    ): Promise<Credentials> {
        const url = `${this.baseUrl}/_synapse/admin/v1/register`;
        const ctx = this.requestContext();

        // Fetch the nonce
        const nonceResp = await ctx.fetch(url, {method: "GET"});
        if (!nonceResp.ok()) {
            throw new Error(`Nonce GET failed: ${nonceResp.status()} ${await nonceResp.text()}`);
        }
        const {nonce} = (await nonceResp.json()) as {nonce: string};

        // Sign the request with the shared secret
        const mac = crypto
            .createHmac("sha1", this.registrationSharedSecret)
            .update(`${nonce}\0${username}\0${password}\0${admin ? "admin" : "notadmin"}`)
            .digest("hex");

        // POST the registration
        const resp = await ctx.fetch(url, {
            method: "POST",
            data: {nonce, username, password, admin, mac, displayname: displayName},
        });
        if (!resp.ok()) {
            throw new Error(`Register POST failed: ${resp.status()} ${await resp.text()}`);
        }
        const data = (await resp.json()) as {
            user_id: string;
            access_token: string;
            device_id: string;
            home_server?: string;
        };

        return {
            homeServer: data.home_server || data.user_id.split(":").slice(1).join(":"),
            homeserverBaseUrl: this.baseUrl,
            accessToken: data.access_token,
            userId: data.user_id,
            deviceId: data.device_id,
            password,
            displayName,
            username,
        };
    }

    /**
     * Registers a non-admin user.
     * @param username The localpart of the user to register.
     * @param password The password to set.
     * @param displayName The optional display name to set.
     * @returns The credentials of the newly-registered user.
     */
    public registerUser(username: string, password: string, displayName?: string): Promise<Credentials> {
        return this.registerUserInternal(username, password, displayName, false);
    }

    /**
     * Logs an existing user in.
     * @param userId The user ID to log in as.
     * @param password The user's password.
     * @returns The credentials of the logged-in user.
     */
    public async loginUser(userId: string, password: string): Promise<Credentials> {
        return {
            ...(await this.csApi.loginUser(userId, password)),
            homeserverBaseUrl: this.baseUrl,
        };
    }

    /**
     * Binds a 3pid to a user. Not implemented: tuwunel does not expose a
     * Synapse-style PUT /_synapse/admin/v2/users/... endpoint.
     * @throws Error Always; specs depending on this must be skipped.
     */
    public async setThreepid(_userId: string, _medium: string, _address: string): Promise<void> {
        throw new Error("setThreepid: not implemented; spec must be skipped"); // XXX: no admin v2 user endpoint
    }
}
