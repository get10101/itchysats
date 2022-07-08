import {useLocation, useNavigate} from "react-router-dom";
import {useAuth} from "../authProvider";
import * as React from "react";
import {ChangeEvent, useState} from "react";
import {Button, Input, InputGroup, InputRightElement} from "@chakra-ui/react";

export default function LoginPage() {
    let navigate = useNavigate();
    let location = useLocation();
    let auth = useAuth();

    const [password, setPassword] = useState<string>("");
    const [show, setShow] = useState(false)

    let from = location.state?.from?.pathname || "/";

    if (auth.isAuthenticated) {
        if (from === "/login") {
            navigate("/long");
        } else {
            navigate(from, { replace: true });
        }
        return;

    }

    function handleSubmit(event: React.FormEvent<HTMLFormElement>) {
        event.preventDefault();

        auth.signin(password, () => {
            // Send them back to the page they tried to visit when they were
            // redirected to the login page. Use { replace: true } so we don't create
            // another entry in the history stack for the login page.  This means that
            // when they get to the protected page and click the back button, they
            // won't end up back on the login page, which is also really nice for the
            // user experience.
            navigate(from, { replace: true });
        });
    }

    const onChange = (event: ChangeEvent<HTMLInputElement>) => {
        setPassword(event.target.value);
    }

    const toggleShowPassword = () => {
        setShow(!show);
    }

    return (
        <>
            <p>You must log in to view the page at {from}</p>

            <form onSubmit={handleSubmit}>
                <InputGroup size='md'>
                    <Input
                        pr='4.5rem'
                        type={show ? 'text' : 'password'}
                        value={password}
                        placeholder='Enter password'
                        onChange={onChange}
                    />
                    <InputRightElement width='4.5rem'>
                        <Button h='1.75rem' size='sm' onClick={toggleShowPassword}>
                            {show ? 'Hide' : 'Show'}
                        </Button>
                    </InputRightElement>
                </InputGroup>
                <Button type="submit">Login</Button>
            </form>
        </>
    );
}
