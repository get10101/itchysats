import axios, { AxiosResponse } from "axios";

export interface User {
    expired: boolean;
    id: number;
}

export interface HttpError {
    title: string;
    detail: string;
}

export async function login(params: {
    password: string;
}): Promise<User> {
    try {
        const loginParams = new URLSearchParams();
        loginParams.append("password", params.password);

        const response: AxiosResponse<User> = await axios.post("/api/login", loginParams);
        if (response.status === 200) {
            return response.data;
        }
    } catch (error: any) {
        return Promise.reject(error.response.data as HttpError);
    }
    return Promise.reject("Could not login");
}

export async function logout() {
    try {
        await axios.get("/api/logout");
    } catch (error: any) {
        return Promise.reject(error.response.data as HttpError);
    }
    return;
}

export interface Authenticated {
    authenticated: boolean;
    first_login: boolean;
}

export async function isAuthenticated(): Promise<Authenticated> {
    try {
        const response: AxiosResponse<Authenticated> = await axios.get("/api/am-I-authenticated");
        if (response.status === 200) {
            return response.data;
        }
    } catch (error: any) {
        return Promise.reject(error.response.data as HttpError);
    }
    return {
        authenticated: false,
        first_login: false,
    };
}

export async function changePassword(params: {
    password: string;
}): Promise<void> {
    try {
        const changePasswordParams = new URLSearchParams();
        changePasswordParams.append("password", params.password);

        const response = await axios.post("/api/change-password", changePasswordParams);
        if (response.status === 200) {
            return;
        }
    } catch (error: any) {
        console.log(error.response.data);
        return Promise.reject(error.response.data as HttpError);
    }
}
